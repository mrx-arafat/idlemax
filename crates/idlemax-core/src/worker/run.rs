//! Domain: the Run, the record of one worker on one task from its first launch to the
//! owner's review, with its ten states and the moves allowed between them. Pure.

use serde::{Deserialize, Serialize};

use super::checks::verdict;
use super::report::{CheckFacts, WorkerPhase, WorkerReport, MAX_END_TEXT_CHARS};
use super::{RunId, WorkerSpec};
use crate::protocol::JobUpdate;
use crate::quota::Provider;
use crate::types::{DeviceId, Job, JobId, JobStatus, QuotaSnapshot, UnixMs};

/// Launches in a row that may fail to reach `running` before the Run needs the owner.
pub const START_TRIES: u32 = 3;

/// Where a Run is. The ten shared values of docs/DISPATCHER.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Starting,
    Running,
    Checking,
    Clean,
    NeedsYou,
    Rejected,
    /// The owner stopped it; only the owner's start moves it on (D11).
    Stopped,
    /// Back in the queue; it restarts by itself when there is room (D14).
    Shed,
    Kept,
    Discarded,
}

impl RunState {
    pub const ALL: [RunState; 10] = [
        RunState::Starting,
        RunState::Running,
        RunState::Checking,
        RunState::Clean,
        RunState::NeedsYou,
        RunState::Rejected,
        RunState::Stopped,
        RunState::Shed,
        RunState::Kept,
        RunState::Discarded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Starting => "starting",
            RunState::Running => "running",
            RunState::Checking => "checking",
            RunState::Clean => "clean",
            RunState::NeedsYou => "needs_you",
            RunState::Rejected => "rejected",
            RunState::Stopped => "stopped",
            RunState::Shed => "shed",
            RunState::Kept => "kept",
            RunState::Discarded => "discarded",
        }
    }

    /// The moves of D11, D14 and D16; every other move is refused.
    pub fn may_become(self, next: RunState) -> bool {
        use RunState::*;
        match self {
            Starting => matches!(next, Running | Stopped | Shed | NeedsYou),
            Running => matches!(next, Checking | Stopped | Shed | NeedsYou),
            Checking => matches!(next, Clean | NeedsYou | Rejected | Stopped | Shed),
            Stopped => matches!(next, Shed | Discarded),
            Shed => matches!(next, Starting | Discarded),
            Clean | NeedsYou | Rejected => matches!(next, Kept | Discarded),
            Kept | Discarded => false,
        }
    }
}

impl std::fmt::Display for RunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RunState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RunState::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| format!("unknown run state: {s}"))
    }
}

/// The result of idlemax's own check of a finished task (D8). Kept beside the state, so it
/// stays visible after a keep or a discard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Clean,
    NeedsYou,
    Rejected,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Clean => "clean",
            Verdict::NeedsYou => "needs_you",
            Verdict::Rejected => "rejected",
        }
    }
}

/// What the device found when a launch ended.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunFacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckFacts>,
    /// The text that ended a turn in which no commit was added (A9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_text: Option<String>,
}

/// One worker on one task (the Run of docs/DISPATCHER.md, D10 item 1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: RunId,
    pub device: DeviceId,
    pub project: String,
    /// The Task id.
    pub task: String,
    pub task_title: String,
    /// The Task's source, the "task type" of D10 item 1 (A24).
    pub source: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub provider: Provider,
    pub account: String,
    pub model: String,
    pub session: Option<String>,
    pub state: RunState,
    pub verdict: Option<Verdict>,
    pub concern: Option<String>,
    /// Why the task was picked, copied from the Plan slot at launch (A5).
    pub reason: String,
    /// One line on what was done.
    pub summary: Option<String>,
    pub facts: RunFacts,
    /// The live or last job of this Run.
    pub job: Option<JobId>,
    /// Jobs started for this Run.
    pub launches: u32,
    /// Launches in a row that never reached `running`.
    pub start_tries: u32,
    pub start_ms: UnixMs,
    /// When the worker first reached `running`; None while it never started.
    pub running_ms: Option<UnixMs>,
    pub end_ms: Option<UnixMs>,
    pub updated_ms: UnixMs,
    pub quota_before: Vec<QuotaSnapshot>,
    pub quota_after: Vec<QuotaSnapshot>,
}

/// What a relaunch of a Run needs, kept on the coordinator only: the worker section of its
/// first launch and the brief, which never leave the store except in the worker job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunLaunch {
    pub worker: WorkerSpec,
    pub brief: String,
}

/// The Run after an update of its live `launch` or `resume` job, or None when the update
/// does not move it: another job, an action that runs no agent, or a move the states do not
/// allow (a Run the owner stopped stays stopped).
pub fn after_job(run: &Run, job: &Job, update: &JobUpdate, now_ms: UnixMs) -> Option<Run> {
    let worker = job.spec.worker.as_ref()?;
    if run.job != Some(job.id) {
        return None;
    }
    if !worker.action.is_launch() {
        return after_review(run, worker.action, update, now_ms);
    }
    let report = update.worker.as_deref();
    let mut next = run.clone();
    next.updated_ms = now_ms;
    if let Some(r) = report {
        next.session = r.session.clone().or(next.session);
        next.branch = r.branch.clone().or(next.branch);
        next.worktree = r.worktree.clone().or(next.worktree);
        let end_text = r.end_text.as_ref().map(|t| clip(t, MAX_END_TEXT_CHARS));
        next.facts.end_text = end_text.or(next.facts.end_text);
    }
    let error = || {
        update
            .error
            .clone()
            .unwrap_or_else(|| format!("job {}", update.status))
    };
    // A turn a usage limit cut is not a failed check: back in the queue, state saved (D10.7).
    let limit = report
        .and_then(|r| r.end_text.as_deref())
        .is_some_and(crate::quota::is_limit_message);
    if limit && update.status.is_terminal() && !update.owner_stop {
        return back_in_queue(next, run, "usage limit reached".into(), now_ms);
    }
    let target = match update.status {
        JobStatus::Running | JobStatus::Paused => match report.map(|r| r.phase) {
            Some(WorkerPhase::Running) => RunState::Running,
            Some(WorkerPhase::Checking) => RunState::Checking,
            _ => run.state,
        },
        JobStatus::Killed if update.owner_stop => RunState::Stopped,
        JobStatus::Queued => return back_in_queue(next, run, error(), now_ms),
        JobStatus::Failed | JobStatus::Killed if update.retryable => {
            return back_in_queue(next, run, error(), now_ms)
        }
        JobStatus::Failed | JobStatus::Killed => {
            next.concern = Some(error());
            RunState::NeedsYou
        }
        JobStatus::Succeeded => return judge(next, run.state, report, worker, now_ms),
        JobStatus::Leased | JobStatus::Cancelled | JobStatus::Parked => run.state,
    };
    move_to(next, run.state, target, now_ms)
}

/// The first `max` characters: a device sends `end_text`, so the coordinator bounds it.
fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The owner's keep or discard: a succeeded one ends the Run `kept` or `discarded` with its
/// verdict kept; a failed or killed one leaves state and verdict as they were (D16).
fn after_review(
    run: &Run,
    action: super::WorkerAction,
    update: &JobUpdate,
    now_ms: UnixMs,
) -> Option<Run> {
    if update.status != JobStatus::Succeeded {
        return None;
    }
    let target = match action {
        super::WorkerAction::Discard => RunState::Discarded,
        _ => RunState::Kept,
    };
    if !run.state.may_become(target) {
        return None;
    }
    Some(Run {
        state: target,
        updated_ms: now_ms,
        end_ms: run.end_ms.or(Some(now_ms)),
        ..run.clone()
    })
}

/// A finished launch: the verdict its check facts give (D8), the facts and the summary kept.
fn judge(
    mut next: Run,
    from: RunState,
    report: Option<&WorkerReport>,
    worker: &WorkerSpec,
    now_ms: UnixMs,
) -> Option<Run> {
    let Some(facts) = report.and_then(|r| r.facts.clone()) else {
        next.concern = Some("the device sent no check facts for this task".into());
        next.end_ms = Some(now_ms);
        return move_to(next, from, RunState::NeedsYou, now_ms);
    };
    let judged = verdict(&facts, worker);
    next.verdict = Some(judged.verdict);
    next.concern = judged.concern;
    next.summary = Some(judged.summary);
    next.facts.check = Some(facts);
    next.end_ms = Some(now_ms);
    let target = match judged.verdict {
        Verdict::Clean => RunState::Clean,
        Verdict::NeedsYou => RunState::NeedsYou,
        Verdict::Rejected => RunState::Rejected,
    };
    move_to(next, from, target, now_ms)
}

/// The Run after the sweep ended its job because the lease ran out.
pub fn after_expiry(run: &Run, job: &Job, now_ms: UnixMs) -> Option<Run> {
    let device = job.device.as_deref().unwrap_or("unknown device");
    let update = JobUpdate {
        id: job.id,
        status: JobStatus::Killed,
        retryable: true,
        error: Some(format!("lease expired on {device}")),
        ..Default::default()
    };
    after_job(run, job, &update, now_ms)
}

fn reaches(from: RunState, to: RunState) -> bool {
    from == to
        || from.may_become(to)
        || (from == RunState::Starting && reaches(RunState::Running, to))
        || (from == RunState::Running && RunState::Checking.may_become(to))
}

/// The Run after a coordinator control (`stop_all`, `cancel`) ended its job: the owner
/// asked for it, so it is `stopped` (D11).
pub fn after_owner_stop(run: &Run, job: &Job, now_ms: UnixMs) -> Option<Run> {
    let update = JobUpdate {
        id: job.id,
        status: JobStatus::Killed,
        owner_stop: true,
        ..Default::default()
    };
    after_job(run, job, &update, now_ms)
}

/// `shed`, or `needs_you` once `START_TRIES` launches in a row never reached `running`.
fn back_in_queue(mut next: Run, run: &Run, error: String, now_ms: UnixMs) -> Option<Run> {
    if run.state == RunState::Starting {
        next.start_tries = run.start_tries + 1;
        if next.start_tries >= START_TRIES {
            next.concern = Some(format!("could not be started {START_TRIES} times: {error}"));
            return move_to(next, run.state, RunState::NeedsYou, now_ms);
        }
    }
    move_to(next, run.state, RunState::Shed, now_ms)
}

/// `next` in `target` when `from` may get there. A Run whose `running` or `checking` update
/// was lost passes through those states, the way a launch moves on.
fn move_to(mut next: Run, from: RunState, target: RunState, now_ms: UnixMs) -> Option<Run> {
    if !reaches(from, target) {
        return None;
    }
    if matches!(target, RunState::Running | RunState::Checking) {
        next.start_tries = 0;
        next.running_ms.get_or_insert(now_ms);
    }
    if target == RunState::NeedsYou {
        next.end_ms = Some(now_ms);
    }
    next.state = target;
    Some(next)
}

#[cfg(test)]
mod tests;
