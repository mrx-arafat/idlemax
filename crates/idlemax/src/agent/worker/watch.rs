//! Adapter: the manager's tick. It takes each launch's outcome, watches a briefed worker's
//! turn (done, blocked, gone, out of runtime), saves its member pids, and stops a worker by
//! closing its pane and then waiting until none of its pids is left, killing what stays.

use std::path::Path;
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;
use std::time::Duration;

use idlemax_core::protocol::{JobUpdate, RunningJob};
use idlemax_core::types::{Job, JobId, JobStatus, UnixMs};
use idlemax_core::worker::report::WorkerPhase;

use super::cmd::Cmd;
use super::host::AgentStatus;
use super::steps::Outcome;
use super::{brief_line, End, Phase, Tracked, Workers};
use crate::agent::runner::os::Sig;
use tracing::warn;

/// Ticks in a row that must read the turn as over.
const ENDED_TICKS: u8 = 2;
/// How long the host may fail to answer before a watched worker fails.
const HOST_DOWN_MS: UnixMs = 60_000;

impl Workers {
    /// Each tracked worker, as the heartbeat reports it.
    pub fn running(&self) -> Vec<RunningJob> {
        let mut procs = self.procs.lock().ok();
        self.tracked
            .values()
            .map(|t| {
                let pids: Vec<u32> = t.members.iter().map(|(pid, _)| *pid).collect();
                RunningJob {
                    id: t.job.id,
                    name: t.job.spec.name.clone(),
                    status: if t.paused {
                        JobStatus::Paused
                    } else {
                        JobStatus::Running
                    },
                    pid: t.progress.shell_pid,
                    started_ms: t.clock.started_ms(),
                    mem_mb: t.job.spec.mem_mb,
                    rss_bytes: procs.as_mut().map(|p| p.rss(&pids)),
                    cpu_pct: None,
                    cost: t.job.spec.cost,
                    ai_account: t.job.spec.ai_account.clone(),
                }
            })
            .collect()
    }

    /// True while a worker is tracked, so the agent samples and beats faster.
    pub fn is_tracking(&self) -> bool {
        !self.tracked.is_empty()
    }

    /// The tracked jobs and whether each is paused, for an agent that takes back recovered
    /// workers.
    pub fn jobs(&self) -> Vec<(Job, bool)> {
        let with_start = |t: &Tracked| Job {
            started_ms: Some(t.clock.started_ms()),
            ..t.job.clone()
        };
        self.tracked
            .values()
            .map(|t| (with_start(t), t.paused))
            .collect()
    }

    /// Every saved member pid of every worker, left out of the biggest memory users.
    pub fn member_pids(&self) -> Vec<u32> {
        let members = self.tracked.values().flat_map(|t| t.members.iter());
        members.map(|(pid, _)| *pid).collect()
    }

    /// Advances every worker one step; the updates to send.
    pub fn tick(&mut self, now_ms: UnixMs, awake_ms: i64) -> Vec<JobUpdate> {
        let ids: Vec<JobId> = self.tracked.keys().copied().collect();
        let mut updates = Vec::new();
        for id in ids {
            updates.extend(self.step(id, now_ms, awake_ms));
        }
        self.save_records();
        updates
    }

    fn step(&mut self, id: JobId, now_ms: UnixMs, awake_ms: i64) -> Option<JobUpdate> {
        let t = self.tracked.get_mut(&id)?;
        match &t.phase {
            Phase::Launching { rx, kill, .. } => {
                let outcome = match rx.try_recv() {
                    Ok(outcome) => outcome,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Outcome::Failed {
                        error: "the launch ended without a result".into(),
                        progress: t.progress.clone(),
                    },
                };
                let kill = kill.clone();
                self.launched(id, outcome, kill, now_ms)
            }
            Phase::Held => self.held(id, now_ms),
            Phase::Watching { .. } => self.watch(id, now_ms, awake_ms),
            Phase::Stopping { .. } => self.stopping(id, now_ms),
            Phase::Checking { .. } => self.checking(id, now_ms),
            Phase::Review { .. } => self.reviewed(id, now_ms),
        }
    }

    fn launched(
        &mut self,
        id: JobId,
        outcome: Outcome,
        kill: Option<End>,
        now_ms: UnixMs,
    ) -> Option<JobUpdate> {
        let t = self.tracked.get_mut(&id)?;
        let held = matches!(outcome, Outcome::Held(_));
        let (progress, end) = match outcome {
            Outcome::Briefed(p) | Outcome::Held(p) => (p, kill),
            Outcome::Failed { error, progress } => {
                (progress, Some(kill.unwrap_or(End::failed(error))))
            }
            Outcome::Cancelled(p) => (
                p,
                Some(kill.unwrap_or(End::failed("the launch was cancelled"))),
            ),
        };
        t.progress = progress;
        match end {
            Some(end) => {
                self.begin_stop(id, end, now_ms);
                None
            }
            None if held => {
                t.phase = Phase::Held;
                self.save_members(id);
                if self.tracked.get(&id).is_some_and(|t| t.paused) {
                    let pids = self.pids_of(id);
                    if let Ok(mut procs) = self.procs.lock() {
                        procs.signal(&pids, Sig::Stop);
                    }
                }
                None
            }
            None => Some(self.briefed(id, now_ms)),
        }
    }

    fn briefed(&mut self, id: JobId, now_ms: UnixMs) -> JobUpdate {
        if let Some(t) = self.tracked.get_mut(&id) {
            t.phase = Phase::Watching {
                saw_working: false,
                ended: 0,
                blocked_since: None,
                host_down_since: None,
            };
        }
        let t = &self.tracked[&id];
        self.update(t, JobStatus::Running, WorkerPhase::Running, now_ms)
    }

    /// A worker whose brief waited for a pause: once the pause is lifted and the agent reads
    /// `idle`, it gets its brief and is watched like any other.
    fn held(&mut self, id: JobId, now_ms: UnixMs) -> Option<JobUpdate> {
        let t = self.tracked.get(&id)?;
        if t.paused {
            return None;
        }
        let handle = t.progress.handle.clone()?;
        let folder = t.progress.folder.clone()?;
        let line = brief_line(t.worker.action, &folder);
        let host = Arc::clone(&t.host);
        match host.state(&handle).ok()?.status {
            AgentStatus::Idle => {}
            AgentStatus::Gone => {
                self.begin_stop(
                    id,
                    End::failed("the agent exited before it finished"),
                    now_ms,
                );
                return None;
            }
            _ => return None,
        }
        if let Err(e) = host.send(&handle, &line) {
            let end = End::failed(format!("the brief could not be sent: {e}"));
            self.begin_stop(id, end, now_ms);
            return None;
        }
        Some(self.briefed(id, now_ms))
    }

    fn watch(&mut self, id: JobId, now_ms: UnixMs, awake_ms: i64) -> Option<JobUpdate> {
        self.save_members(id);
        let t = self.tracked.get(&id)?;
        if t.paused {
            return None;
        }
        let limit_secs = t.job.spec.max_runtime_secs;
        if t.clock.active_ms(awake_ms) > i64::try_from(limit_secs.saturating_mul(1000)).ok()? {
            let mut end = End::failed(format!("ran longer than {limit_secs} s"));
            end.status = JobStatus::Killed;
            self.begin_stop(id, end, now_ms);
            return None;
        }
        let handle = t.progress.handle.clone()?;
        let state = match t.host.state(&handle) {
            Ok(state) => state,
            Err(e) => return self.host_down(id, &e.to_string(), now_ms),
        };
        // A headless agent that exited with an error ended its turn in failure; its last
        // lines may hold the limit message Spec 3 reads (A9).
        if let (AgentStatus::Done, Some(code)) = (state.status, state.exit_code) {
            if code != 0 {
                let mut end = End::failed(format!("the agent exited with code {code}"));
                end.end_text = t.host.end_text(&handle);
                self.begin_stop(id, end, now_ms);
                return None;
            }
        }
        let blocked_ms = i64::try_from(t.worker.blocked_secs.saturating_mul(1000)).ok()?;
        let t = self.tracked.get_mut(&id)?;
        if state.session.is_some() {
            t.progress.session = state.session;
        }
        let Phase::Watching {
            saw_working,
            ended,
            blocked_since,
            host_down_since,
        } = &mut t.phase
        else {
            return None;
        };
        *host_down_since = None;
        *saw_working |= state.status == AgentStatus::Working;
        *blocked_since = match state.status {
            AgentStatus::Blocked => Some(blocked_since.unwrap_or(now_ms)),
            _ => None,
        };
        let over = match state.status {
            AgentStatus::Done => true,
            AgentStatus::Idle => *saw_working,
            _ => false,
        };
        *ended = if over { *ended + 1 } else { 0 };
        let waited = blocked_since.is_some_and(|since| now_ms - since > blocked_ms);
        let finished = *ended >= ENDED_TICKS;
        let end = match state.status {
            AgentStatus::Gone => Some(End::failed("the agent exited before it finished")),
            _ if waited => Some(End::failed("the worker waited for input")),
            _ if finished => Some(self.turn_end(id)),
            _ => None,
        };
        if let Some(end) = end {
            self.begin_stop(id, end, now_ms);
        }
        None
    }

    /// The host did not answer: logged once, and after `HOST_DOWN_MS` the job fails like a
    /// vanished pane, since no end of the turn can be seen.
    fn host_down(&mut self, id: JobId, why: &str, now_ms: UnixMs) -> Option<JobUpdate> {
        let t = self.tracked.get_mut(&id)?;
        let Phase::Watching {
            host_down_since, ..
        } = &mut t.phase
        else {
            return None;
        };
        let since = match host_down_since {
            Some(since) => *since,
            None => {
                warn!(job = id, error = why, "the worker's host does not answer");
                *host_down_since = Some(now_ms);
                now_ms
            }
        };
        if now_ms - since > HOST_DOWN_MS {
            let end = End::failed(format!(
                "herdr did not answer for {} s: {why}",
                HOST_DOWN_MS / 1000
            ));
            self.begin_stop(id, end, now_ms);
        }
        None
    }

    /// A finished turn; the host's `end_text` when this launch added no commit (A9).
    fn turn_end(&self, id: JobId) -> End {
        let mut end = End {
            status: JobStatus::Succeeded,
            error: None,
            retryable: false,
            owner_stop: false,
            end_text: None,
        };
        let Some(t) = self.tracked.get(&id) else {
            return end;
        };
        let committed = match (&t.progress.folder, &t.progress.start_head) {
            (Some(folder), Some(base)) => self.commits_since(folder, base) > 0,
            _ => false,
        };
        if !committed {
            end.end_text = t.progress.handle.as_ref().and_then(|h| t.host.end_text(h));
        }
        end
    }

    fn commits_since(&self, folder: &Path, base: &str) -> u64 {
        let range = format!("{base}..HEAD");
        let args = ["rev-list", "--count", range.as_str()];
        let cmd = Cmd::git(folder, &args, Duration::from_secs(10));
        let out = self.commands.run(&cmd).ok();
        let out = out.filter(|o| o.code == Some(0));
        out.and_then(|o| o.stdout.trim().parse().ok()).unwrap_or(0)
    }

    /// Saves the worker's member pids with their start times, and puts each new one in the
    /// background class (A16), so a stop never depends on Herdr.
    fn save_members(&mut self, id: JobId) {
        let Some(t) = self.tracked.get(&id) else {
            return;
        };
        let Some(shell) = t.progress.shell_pid else {
            return;
        };
        let (tag, started) = (t.tag, t.clock.started_ms());
        let Some(members) = self
            .procs
            .lock()
            .ok()
            .map(|mut p| p.members(shell, tag, started))
        else {
            return;
        };
        let fresh: Vec<u32> = members
            .iter()
            .map(|(pid, _)| *pid)
            .filter(|pid| !t.throttled.contains(pid))
            .collect();
        for pid in &fresh {
            self.throttle(*pid);
        }
        if let Some(t) = self.tracked.get_mut(&id) {
            t.throttled.extend(fresh);
            if !members.is_empty() {
                t.members = members;
            }
        }
    }

    fn throttle(&self, pid: u32) {
        let Some(program) = &self.background else {
            return;
        };
        let cmd = Cmd {
            program: program.clone(),
            args: vec!["-b".into(), "-p".into(), pid.to_string()],
            cwd: None,
            env: Vec::new(),
            stdin: None,
            limit: Duration::from_secs(10),
            throttle: false,
            tag: None,
        };
        let _ = self.commands.run(&cmd);
    }

    /// Thaws a paused worker and closes its pane; `stopping` waits for its pids to go.
    pub(super) fn begin_stop(&mut self, id: JobId, end: End, now_ms: UnixMs) {
        self.save_members(id);
        let pids = self.pids_of(id);
        let Some(t) = self.tracked.get_mut(&id) else {
            return;
        };
        if t.paused {
            if let Ok(mut procs) = self.procs.lock() {
                procs.signal(&pids, Sig::Cont);
            }
        }
        let closed = match &t.progress.handle {
            Some(handle) => t.host.stop(handle).is_ok(),
            None => true,
        };
        t.phase = Phase::Stopping {
            since_ms: now_ms,
            end,
            forced: !closed,
        };
    }
}
