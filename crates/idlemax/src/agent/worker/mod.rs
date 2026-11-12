//! Adapter: the worker manager. It carries each leased worker job from its lease to its exit:
//! the launch on its own thread (`steps`), then a tick that watches the turn, applies the
//! limits, and stops the worker by its saved pids without depending on Herdr (`watch`).
//! Pause, resume and kill reach it as messages, the same decisions the runner gets.

mod check;
pub mod cmd;
mod end;
mod finish;
pub mod herdr;
pub mod host;
pub mod procs;
mod record;
mod steps;
#[cfg(test)]
mod tests;
mod watch;
// Its safe remove and branch delete serve `idlemax review` (dispatcher Spec 2 Task 11).
#[cfg_attr(not(test), allow(dead_code))]
pub mod worktree;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use idlemax_core::job_run::{allows_program, RunClock};
use idlemax_core::protocol::{JobUpdate, RunningJob};
use idlemax_core::types::{DeviceId, Job, JobId, JobStatus, QuotaSnapshot, UnixMs};
use idlemax_core::worker::report::{CheckFacts, WorkerPhase, WorkerReport};
use idlemax_core::worker::{WorkerAction, WorkerCli, WorkerSpec};
use idlemax_core::Paths;

use crate::agent::runner::os::Sig;
use cmd::Commands;
use end::End;
pub use end::Msg;
use host::WorkerHost;
use procs::{Member, Procs};
use steps::{brief_line, LaunchCtx, Outcome, Progress};

enum Phase {
    Launching {
        rx: Receiver<Outcome>,
        cancel: Arc<AtomicBool>,
        /// The owner's pause, which the launch thread obeys between its phases.
        paused: Arc<AtomicBool>,
        /// The kill that arrived while the launch was still running.
        kill: Option<End>,
    },
    /// The agent runs, frozen, and gets its brief once the pause is lifted.
    Held,
    Watching {
        saw_working: bool,
        /// Ticks in a row that read the turn as over.
        ended: u8,
        blocked_since: Option<UnixMs>,
        /// Since when the host has not answered.
        host_down_since: Option<UnixMs>,
    },
    Stopping {
        since_ms: UnixMs,
        end: End,
        /// The pane could not be closed, so the pids are killed at once.
        forced: bool,
    },
    /// The pane is closed and its processes are gone; the facts of the turn are gathered.
    Checking {
        rx: Receiver<CheckFacts>,
        cancel: Arc<AtomicBool>,
        end: End,
    },
}

struct Tracked {
    job: Job,
    worker: WorkerSpec,
    tag: u64,
    clock: RunClock,
    paused: bool,
    progress: Progress,
    members: Vec<Member>,
    throttled: BTreeSet<u32>,
    phase: Phase,
    /// The record last saved, so an unchanged worker is not written again.
    saved: Option<String>,
}

pub struct Workers {
    host: Arc<dyn WorkerHost>,
    commands: Arc<dyn Commands>,
    procs: Arc<Mutex<dyn Procs>>,
    paths: Paths,
    device: DeviceId,
    allowlist: Vec<String>,
    /// The program that puts a pid in the background class (A16).
    background: Option<String>,
    /// How often a launch reads the agent's status while it waits for `idle`.
    poll: Duration,
    /// How long an agent may take to reach `idle`.
    idle_wait: Duration,
    /// The agent's newest quota readings, so a review counts for the account that paid.
    readings: Arc<Mutex<Vec<QuotaSnapshot>>>,
    tracked: BTreeMap<JobId, Tracked>,
}

impl Workers {
    pub fn new(
        host: Arc<dyn WorkerHost>,
        commands: Arc<dyn Commands>,
        procs: Arc<Mutex<dyn Procs>>,
        paths: Paths,
        device: DeviceId,
    ) -> Self {
        let background = cfg!(target_os = "macos").then(|| "/usr/sbin/taskpolicy".to_string());
        Self {
            host,
            commands,
            procs,
            paths,
            device,
            allowlist: Vec::new(),
            background,
            poll: Duration::from_secs(1),
            idle_wait: Duration::from_secs(60),
            readings: Arc::new(Mutex::new(Vec::new())),
            tracked: BTreeMap::new(),
        }
    }

    /// The agent's newest quota readings.
    pub fn set_readings(&mut self, readings: Vec<QuotaSnapshot>) {
        if let Ok(mut held) = self.readings.lock() {
            *held = readings;
        }
    }

    /// `agent.command_allowlist`, which a reload may change.
    pub fn set_allowlist(&mut self, allowlist: Vec<String>) {
        self.allowlist = allowlist;
    }

    /// Applies one message; the updates it causes at once.
    pub fn handle(&mut self, msg: Msg, now_ms: UnixMs, awake_ms: i64) -> Vec<JobUpdate> {
        match msg {
            Msg::Start(job) => self.start(*job, now_ms, awake_ms),
            Msg::Pause(id) => self.pause(id, true, now_ms, awake_ms),
            Msg::Resume(id) => self.pause(id, false, now_ms, awake_ms),
            Msg::Kill { id, order } => {
                self.kill(id, End::killed(&order), now_ms);
                Vec::new()
            }
        }
    }

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

    fn start(&mut self, job: Job, now_ms: UnixMs, awake_ms: i64) -> Vec<JobUpdate> {
        let Some(worker) = job.spec.worker.clone() else {
            return vec![self.refusal(&job, "not a worker job", now_ms)];
        };
        if let Some(why) = self.refused(&worker) {
            return vec![self.refusal(&job, &why, now_ms)];
        }
        let session = match (worker.action, worker.cli) {
            (WorkerAction::Resume, _) => worker.session.clone(),
            (_, WorkerCli::Claude) => match crate::state::new_session_id() {
                Ok(id) => Some(id),
                Err(e) => return vec![self.refusal(&job, &e.to_string(), now_ms)],
            },
            _ => None,
        };
        let tag = new_tag(job.id, now_ms);
        let cancel = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let rx = steps::spawn(LaunchCtx {
            host: Arc::clone(&self.host),
            commands: Arc::clone(&self.commands),
            procs: Arc::clone(&self.procs),
            worktrees_dir: self.paths.worktrees_dir(),
            job: job.clone(),
            worker: worker.clone(),
            tag,
            session: session.clone(),
            cancel: Arc::clone(&cancel),
            paused: Arc::clone(&paused),
            now_ms,
            poll: self.poll,
            idle_wait: self.idle_wait,
        });
        let tracked = Tracked {
            job,
            worker,
            tag,
            clock: RunClock::start(now_ms, awake_ms),
            paused: false,
            progress: Progress {
                session,
                ..Default::default()
            },
            members: Vec::new(),
            throttled: BTreeSet::new(),
            phase: Phase::Launching {
                rx,
                cancel,
                paused,
                kill: None,
            },
            saved: None,
        };
        let update = self.update(&tracked, JobStatus::Running, WorkerPhase::Preparing, now_ms);
        self.tracked.insert(tracked.job.id, tracked);
        vec![update]
    }

    /// Why a worker job may not start here: a program outside `agent.command_allowlist`
    /// (the runner's check, which worker jobs skip), or a worker of its Run still tracked.
    fn refused(&self, w: &WorkerSpec) -> Option<String> {
        let mut programs = vec![w.cli.as_str().to_string()];
        programs.extend(w.reviewer.as_ref().map(|r| r.cli.as_str().to_string()));
        programs.extend(w.setup.first().cloned());
        programs.extend(w.check.first().cloned());
        if let Some(p) = programs
            .iter()
            .find(|p| !allows_program(&self.allowlist, p))
        {
            return Some(format!("command {p:?} is not in agent.command_allowlist"));
        }
        self.tracked
            .values()
            .any(|t| t.worker.run == w.run)
            .then(|| format!("a worker of run {} is still running here", w.run))
    }

    fn refusal(&self, job: &Job, why: &str, now_ms: UnixMs) -> JobUpdate {
        JobUpdate {
            id: job.id,
            device: self.device.clone(),
            attempt: job.attempts,
            status: JobStatus::Failed,
            at_ms: now_ms,
            error: Some(why.to_string()),
            ..Default::default()
        }
    }

    fn pause(&mut self, id: JobId, pause: bool, now_ms: UnixMs, awake_ms: i64) -> Vec<JobUpdate> {
        let Some(t) = self.tracked.get_mut(&id) else {
            return Vec::new();
        };
        if t.paused == pause || matches!(t.phase, Phase::Stopping { .. }) {
            return Vec::new();
        }
        t.paused = pause;
        if let Phase::Launching { paused, .. } = &t.phase {
            paused.store(pause, Ordering::SeqCst);
        }
        let pids = self.pids_of(id);
        let (sig, status) = match pause {
            true => (Sig::Stop, JobStatus::Paused),
            false => (Sig::Cont, JobStatus::Running),
        };
        if let Ok(mut procs) = self.procs.lock() {
            procs.signal(&pids, sig);
        }
        let Some(t) = self.tracked.get_mut(&id) else {
            return Vec::new();
        };
        match pause {
            true => t.clock.pause(awake_ms),
            false => t.clock.resume(awake_ms),
        }
        let phase = match t.phase {
            Phase::Watching { .. } => WorkerPhase::Running,
            Phase::Checking { .. } => WorkerPhase::Checking,
            _ => WorkerPhase::Preparing,
        };
        let t = &self.tracked[&id];
        vec![self.update(t, status, phase, now_ms)]
    }

    /// The live pids of a worker: its members, and during a launch its tagged setup or check.
    fn pids_of(&self, id: JobId) -> Vec<u32> {
        let Some(t) = self.tracked.get(&id) else {
            return Vec::new();
        };
        match self.procs.lock() {
            Ok(mut procs) => procs.alive(&t.members, t.tag, t.clock.started_ms()),
            Err(_) => Vec::new(),
        }
    }

    fn kill(&mut self, id: JobId, end: End, now_ms: UnixMs) {
        let pids = self.pids_of(id);
        let Some(t) = self.tracked.get_mut(&id) else {
            return;
        };
        match &mut t.phase {
            Phase::Launching { cancel, kill, .. } => {
                cancel.store(true, Ordering::SeqCst);
                match kill {
                    Some(first) => first.merge(end),
                    None => *kill = Some(end),
                }
                // A running setup or check ends now; the launch thread then returns.
                if let Ok(mut procs) = self.procs.lock() {
                    procs.signal(&pids, Sig::Cont);
                    procs.signal(&pids, Sig::Kill);
                }
            }
            Phase::Watching { .. } | Phase::Held => self.begin_stop(id, end, now_ms),
            Phase::Stopping { end: first, .. } => first.merge(end),
            Phase::Checking { .. } => self.kill_check(id, end),
        }
    }

    fn update(&self, t: &Tracked, status: JobStatus, phase: WorkerPhase, now: UnixMs) -> JobUpdate {
        JobUpdate {
            id: t.job.id,
            device: self.device.clone(),
            attempt: t.job.attempts,
            status,
            at_ms: now,
            worker: Some(Box::new(WorkerReport {
                run: t.worker.run,
                phase,
                session: t.progress.session.clone(),
                branch: t.progress.branch.clone(),
                worktree: t.progress.folder.as_ref().map(|f| f.display().to_string()),
                ..Default::default()
            })),
            ..Default::default()
        }
    }
}

/// A random tag for one worker attempt, never 0 (see the runner's tag).
fn new_tag(id: JobId, now_ms: UnixMs) -> u64 {
    use std::hash::BuildHasher;
    let tag = std::collections::hash_map::RandomState::new().hash_one((id, now_ms));
    tag.max(1)
}
