//! Adapter: the end of a worker's life. Once its pane is closed and none of its pids is left,
//! a finished turn is checked on a thread of its own (`check`) and the terminal update carries
//! the facts; any other end is reported at once. A kill during the check still lands.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;

use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{JobId, JobStatus, UnixMs};
use idlemax_core::worker::report::{CheckFacts, WorkerPhase};

use super::check::{self, CheckCtx};
use super::gitdir;
use super::{End, Phase, Workers};
use crate::agent::runner::os::Sig;

/// What is left of a worker this long after its pane was closed is killed by pid.
const STOP_GRACE_MS: UnixMs = 5_000;

impl Workers {
    /// The terminal update once no pid of the worker is left; what outlives the grace, or a
    /// pane that could not be closed, is killed by pid. A finished turn is checked first.
    pub(super) fn stopping(&mut self, id: JobId, now_ms: UnixMs) -> Option<JobUpdate> {
        let pids = self.pids_of(id);
        let t = self.tracked.get(&id)?;
        let Phase::Stopping {
            since_ms,
            end,
            forced,
        } = &t.phase
        else {
            return None;
        };
        if !pids.is_empty() {
            if *forced || now_ms - since_ms >= STOP_GRACE_MS {
                if let Ok(mut procs) = self.procs.lock() {
                    procs.signal(&pids, Sig::Kill);
                }
            }
            return None;
        }
        let end = end.clone();
        // Before any git of idlemax's runs again in the repository: undo the worker's change.
        let git_changed = t.progress.git.as_ref().is_some_and(gitdir::undo_change);
        if end.status == JobStatus::Succeeded {
            return self.begin_check(id, end, git_changed, now_ms);
        }
        let facts = git_changed.then(|| CheckFacts {
            git_changed,
            ..Default::default()
        });
        Some(self.finish(id, end, facts, now_ms))
    }

    /// Starts gathering the facts of a finished turn and reports the `checking` phase.
    fn begin_check(
        &mut self,
        id: JobId,
        end: End,
        git_changed: bool,
        now_ms: UnixMs,
    ) -> Option<JobUpdate> {
        let t = self.tracked.get(&id)?;
        let (Some(folder), Some(base)) =
            (t.progress.folder.clone(), t.progress.base_commit.clone())
        else {
            let end = End::failed("the worktree of the finished turn is unknown");
            return Some(self.finish(id, end, None, now_ms));
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let rx = check::spawn(CheckCtx {
            commands: Arc::clone(&self.commands),
            worker: t.worker.clone(),
            job: t.job.id,
            tag: t.tag,
            folder,
            branch: t.progress.branch.clone().unwrap_or_default(),
            base_commit: base,
            git: t.progress.git.clone(),
            git_changed,
            remotes: t.progress.remotes.clone(),
            readings: Arc::clone(&self.readings),
            cancel: Arc::clone(&cancel),
            state_dir: self.paths.root().to_path_buf(),
            guarded: t.guarded.clone(),
        });
        let t = self.tracked.get_mut(&id)?;
        t.phase = Phase::Checking { rx, cancel, end };
        let t = &self.tracked[&id];
        Some(self.update(t, JobStatus::Running, WorkerPhase::Checking, now_ms))
    }

    /// The facts, once the check thread has them.
    pub(super) fn checking(&mut self, id: JobId, now_ms: UnixMs) -> Option<JobUpdate> {
        let t = self.tracked.get(&id)?;
        let Phase::Checking { rx, end, .. } = &t.phase else {
            return None;
        };
        let facts = match rx.try_recv() {
            Ok(facts) => Some(facts),
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => None,
        };
        let end = end.clone();
        // A kill during the check ends the job as killed; its facts are not a verdict, unless
        // they say the git config or hooks changed, which rejects the Run however it ended.
        let facts = facts.filter(|f| end.status == JobStatus::Succeeded || f.git_changed);
        Some(self.finish(id, end, facts, now_ms))
    }

    /// A kill while the check runs: the check's own commands end by their tag.
    pub(super) fn kill_check(&mut self, id: JobId, end: End) {
        let pids = self.pids_of(id);
        let Some(t) = self.tracked.get_mut(&id) else {
            return;
        };
        if let Phase::Checking {
            cancel, end: first, ..
        } = &mut t.phase
        {
            cancel.store(true, Ordering::SeqCst);
            let end_text = first.end_text.take();
            *first = end;
            first.end_text = end_text;
        }
        if let Ok(mut procs) = self.procs.lock() {
            procs.signal(&pids, Sig::Kill);
        }
    }

    /// The result of the owner's keep, merge or discard, once its thread has it.
    pub(super) fn reviewed(&mut self, id: JobId, now_ms: UnixMs) -> Option<JobUpdate> {
        let t = self.tracked.get(&id)?;
        let Phase::Review { rx } = &t.phase else {
            return None;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the review ended without a result".into()),
        };
        let end = match result {
            Ok(_) => End {
                status: JobStatus::Succeeded,
                error: None,
                retryable: false,
                owner_stop: false,
                end_text: None,
            },
            Err(why) => End::failed(why),
        };
        Some(self.finish(id, end, None, now_ms))
    }

    fn finish(
        &mut self,
        id: JobId,
        end: End,
        facts: Option<CheckFacts>,
        now_ms: UnixMs,
    ) -> JobUpdate {
        let t = &self.tracked[&id];
        if let Some(s) = &t.progress.git {
            gitdir::discard(s);
        }
        let mut update = self.update(t, end.status, WorkerPhase::Done, now_ms);
        update.error = end.error;
        update.retryable = end.retryable;
        update.owner_stop = end.owner_stop;
        if let Some(report) = update.worker.as_mut() {
            report.end_text = end.end_text;
            report.facts = facts;
        }
        self.tracked.remove(&id);
        self.forget_record(id);
        update
    }
}
