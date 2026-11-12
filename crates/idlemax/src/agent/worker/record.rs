//! Adapter: the saved record of each live worker (`agent/workers/<job id>.json`, 0600), so a
//! restarted agent takes back a worker whose pane is still its own and costs no session
//! reload, and reports one that is lost as a retryable kill so its Run resumes by itself.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use idlemax_core::job_run::RunClock;
use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{Job, JobId, JobStatus, UnixMs};
use idlemax_core::worker::report::{CheckFacts, WorkerPhase, WorkerReport};
use serde::{Deserialize, Serialize};

use super::gitdir::{self, GitSnapshot};
use super::host::{Handle, HostKind};
use super::procs::Member;
use super::steps::Progress;
use super::{Phase, Tracked, Workers};
use crate::agent::runner::os::Sig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    /// The job without its brief.
    job: Job,
    tag: u64,
    started_ms: UnixMs,
    briefed: bool,
    /// The host that started the worker; a record without it is a Herdr one.
    #[serde(default)]
    host: HostKind,
    session: Option<String>,
    branch: Option<String>,
    folder: Option<PathBuf>,
    base_commit: Option<String>,
    #[serde(default)]
    start_head: Option<String>,
    /// Frozen by a pause when it was saved; it comes back paused.
    #[serde(default)]
    paused: bool,
    pane: Option<String>,
    workspace: Option<String>,
    label: Option<String>,
    shell: Option<Member>,
    members: Vec<Member>,
    /// The repository's git config and hooks before the worker started.
    #[serde(default)]
    git: Option<GitSnapshot>,
}

impl Workers {
    fn records_dir(&self) -> PathBuf {
        self.paths.agent_state_dir().join("workers")
    }

    fn record_path(&self, id: JobId) -> PathBuf {
        self.records_dir().join(format!("{id}.json"))
    }

    /// Writes the record of every worker whose saved facts changed.
    pub(super) fn save_records(&mut self) {
        if crate::state::create_private_dir(&self.records_dir()).is_err() {
            return;
        }
        let ids: Vec<JobId> = self.tracked.keys().copied().collect();
        for id in ids {
            let Some(t) = self.tracked.get(&id) else {
                continue;
            };
            let Ok(text) = serde_json::to_string(&record_of(t)) else {
                continue;
            };
            if t.saved.as_deref() == Some(text.as_str()) {
                continue;
            }
            if write_private(&self.record_path(id), &text).is_ok() {
                if let Some(t) = self.tracked.get_mut(&id) {
                    t.saved = Some(text);
                }
            }
        }
    }

    pub(super) fn forget_record(&self, id: JobId) {
        let _ = std::fs::remove_file(self.record_path(id));
    }

    /// Takes back the workers a previous agent left. One whose pane is still listed with the
    /// saved label and workspace, and whose shell is the same process, is watched again; any
    /// other has its pane closed when it matches, its tagged leftovers killed, and its job
    /// reported `killed` and retryable. A headless worker is a child of the agent that ended
    /// with it, so it is never taken back: what its tag still finds is killed and its Run
    /// continues with `--resume`.
    pub fn recover(&mut self, now_ms: UnixMs, awake_ms: i64) -> Vec<JobUpdate> {
        let Ok(entries) = std::fs::read_dir(self.records_dir()) else {
            return Vec::new();
        };
        let mut updates = Vec::new();
        for path in entries.flatten().map(|e| e.path()) {
            let record = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Record>(&text).ok());
            let Some(record) = record else {
                let _ = std::fs::remove_file(&path);
                continue;
            };
            if self.still_ours(&record) {
                self.adopt(record, awake_ms);
                continue;
            }
            updates.push(self.lost(record, now_ms));
            let _ = std::fs::remove_file(&path);
        }
        updates
    }

    fn still_ours(&self, r: &Record) -> bool {
        let (Some(handle), Some(shell)) = (handle_of(r), r.shell) else {
            return false;
        };
        if r.host == HostKind::Headless {
            return false;
        }
        let pane_ok = r.briefed && self.host.matches(&handle).unwrap_or(false);
        let shell_ok = shell.1.is_some()
            && self
                .procs
                .lock()
                .is_ok_and(|mut p| p.alive(&[shell], 0, r.started_ms).contains(&shell.0));
        pane_ok && shell_ok
    }

    fn adopt(&mut self, r: Record, awake_ms: i64) {
        let Some(worker) = r.job.spec.worker.clone() else {
            return;
        };
        let tracked = Tracked {
            worker,
            host: Arc::clone(&self.host),
            kind: HostKind::Herdr,
            tag: r.tag,
            // The awake time before the restart is not known, so the runtime cap restarts.
            clock: RunClock::start(r.started_ms, awake_ms),
            paused: r.paused,
            progress: Progress {
                folder: r.folder.clone(),
                branch: r.branch.clone(),
                base_commit: r.base_commit.clone(),
                handle: handle_of(&r),
                session: r.session.clone(),
                shell_pid: r.shell.map(|(pid, _)| pid),
                remotes: Vec::new(),
                start_head: r.start_head.clone(),
                git: r.git.clone(),
            },
            members: r.members.clone(),
            throttled: r
                .members
                .iter()
                .map(|(pid, _)| *pid)
                .collect::<BTreeSet<_>>(),
            phase: Phase::Watching {
                // It was briefed, so an idle agent has ended its turn.
                saw_working: true,
                ended: 0,
                blocked_since: None,
                host_down_since: None,
            },
            saved: None,
            sandbox: None,
            guarded: Vec::new(),
            job: r.job,
        };
        let mut tracked = tracked;
        if tracked.paused {
            tracked.clock.pause(awake_ms);
        }
        self.tracked.insert(tracked.job.id, tracked);
    }

    fn lost(&self, r: Record, now_ms: UnixMs) -> JobUpdate {
        let host = match r.host {
            HostKind::Herdr => Some(&self.host),
            HostKind::Headless => self.headless.as_ref(),
        };
        if let (Some(host), Some(handle)) = (host, handle_of(&r)) {
            // `stop` closes the pane only while its label and workspace still match.
            let _ = host.stop(&handle);
        }
        if let Ok(mut procs) = self.procs.lock() {
            let left = procs.alive(&r.members, r.tag, r.started_ms);
            procs.signal(&left, Sig::Kill);
        }
        // A lost worker's change to the git config or hooks rejects its Run like any other.
        let changed = r.git.as_ref().is_some_and(gitdir::undo_change);
        if let Some(s) = &r.git {
            gitdir::discard(s);
        }
        let worker = r.job.spec.worker.as_ref().filter(|_| changed).map(|w| {
            Box::new(WorkerReport {
                run: w.run,
                phase: WorkerPhase::Done,
                facts: Some(CheckFacts {
                    git_changed: true,
                    ..Default::default()
                }),
                ..Default::default()
            })
        });
        JobUpdate {
            id: r.job.id,
            device: self.device.clone(),
            attempt: r.job.attempts,
            status: JobStatus::Killed,
            at_ms: now_ms,
            error: Some("agent restarted; the worker was lost".into()),
            retryable: true,
            worker,
            ..Default::default()
        }
    }
}

fn record_of(t: &Tracked) -> Record {
    let mut job = t.job.clone();
    job.spec.stdin = None;
    let handle = t.progress.handle.as_ref();
    Record {
        job,
        tag: t.tag,
        started_ms: t.clock.started_ms(),
        briefed: !matches!(t.phase, Phase::Launching { .. } | Phase::Held),
        host: t.kind,
        session: t.progress.session.clone(),
        branch: t.progress.branch.clone(),
        folder: t.progress.folder.clone(),
        base_commit: t.progress.base_commit.clone(),
        start_head: t.progress.start_head.clone(),
        paused: t.paused,
        pane: handle.map(|h| h.pane.clone()),
        workspace: handle.map(|h| h.workspace.clone()),
        label: handle.map(|h| h.label.clone()),
        shell: t.progress.shell_pid.map(|pid| {
            let start = t.members.iter().find(|(p, _)| *p == pid).and_then(|m| m.1);
            (pid, start)
        }),
        members: t.members.clone(),
        git: t.progress.git.clone(),
    }
}

fn handle_of(r: &Record) -> Option<Handle> {
    Some(Handle {
        pane: r.pane.clone()?,
        workspace: r.workspace.clone()?,
        label: r.label.clone()?,
    })
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    crate::state::private_file()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?
        .write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_without_a_host_is_a_herdr_one_and_a_headless_one_round_trips() {
        let mut record = Record {
            job: Job::default(),
            tag: 1,
            started_ms: 2,
            briefed: true,
            host: HostKind::Headless,
            session: None,
            branch: None,
            folder: None,
            base_commit: None,
            start_head: None,
            paused: false,
            pane: None,
            workspace: None,
            label: None,
            shell: None,
            members: Vec::new(),
            git: None,
        };
        let mut json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["host"], "headless");
        let back: Record = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.host, HostKind::Headless);
        json.as_object_mut().unwrap().remove("host");
        record.host = HostKind::Herdr;
        assert_eq!(serde_json::from_value::<Record>(json).unwrap(), record);
    }
}
