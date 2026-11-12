//! Worker jobs in the agent tick: a `launch` or `resume` starts by the line state and never by
//! the guard, the shed stops one worker at a time (newest first, then the lowest priority, then
//! the biggest memory user) until the device reads `run` again, and an emergency stops every
//! worker at once (D14, A6, A16). A `keep`, `keep_merge` or `discard` is the owner's own action
//! and nothing holds it (A10). Part of the agent tick decisions (domain layer, pure).

use std::collections::BTreeMap;

use super::{AgentState, KillOrder, TickInput, TrackedJob, SHUTDOWN_REASON};
use crate::dispatch::room::LineState;
use crate::types::JobId;
use crate::worker::WorkerAction;

impl AgentState {
    /// The kills the worker lines order this tick.
    pub(super) fn worker_kills(
        &mut self,
        input: &TickInput<'_>,
        kills: &mut BTreeMap<JobId, KillOrder>,
    ) {
        let line = input.room.state;
        let workers: Vec<&TrackedJob> = self.jobs.values().filter(|j| is_launch(j)).collect();
        if workers.is_empty() || line == LineState::Run {
            self.shedding = false;
        }
        let order = || {
            KillOrder::retryable(format!(
                "line {}: {}; biggest memory users: {}",
                line.as_str(),
                input.room.reason,
                input.others
            ))
        };
        if line == LineState::Emergency {
            for job in workers.iter().filter(|j| j.kill.is_none()) {
                kills.entry(job.id).or_insert_with(order);
            }
            return;
        }
        if line == LineState::Shed {
            self.shedding = true;
        }
        let exiting = workers.iter().any(|j| j.kill.is_some());
        let fresh = self.shed_sample != Some(input.room.at_ms);
        if !self.shedding || line == LineState::Run || exiting || !fresh {
            return;
        }
        if let Some(id) = shed_next(&workers, input.worker_mem) {
            kills.entry(id).or_insert_with(order);
            self.shed_sample = Some(input.room.at_ms);
        }
    }
}

/// The worker a shed stops first: the most recently started, then the lowest priority,
/// then the biggest memory user (D14, A24).
fn shed_next(workers: &[&TrackedJob], mem: &[(JobId, u64)]) -> Option<JobId> {
    let rss = |id: JobId| mem.iter().find(|(j, _)| *j == id).map_or(0, |(_, m)| *m);
    workers
        .iter()
        .filter(|j| j.kill.is_none())
        .max_by(|a, b| {
            a.started_ms
                .cmp(&b.started_ms)
                .then(b.priority.cmp(&a.priority))
                .then(rss(a.id).cmp(&rss(b.id)))
                .then(a.id.cmp(&b.id))
        })
        .map(|j| j.id)
}

/// A worker that runs an agent (`launch` or `resume`).
pub(super) fn is_launch(job: &TrackedJob) -> bool {
    job.worker.is_some_and(WorkerAction::is_launch)
}

/// The owner's keep, merge or discard.
pub(super) fn is_owner_action(job: &TrackedJob) -> bool {
    job.worker.is_some_and(|a| !a.is_launch())
}

/// Why a worker lease may not start now. A launch needs no hard blocker and a line of `run`;
/// an owner action is held only by the agent shutting down.
pub(super) fn worker_blocker(
    input: &TickInput<'_>,
    hard: &Option<String>,
    action: WorkerAction,
) -> Option<String> {
    if !action.is_launch() {
        return input.shutting_down.then(|| SHUTDOWN_REASON.to_string());
    }
    if let Some(r) = hard {
        return Some(r.clone());
    }
    (input.room.state != LineState::Run)
        .then(|| format!("line {}: {}", input.room.state.as_str(), input.room.reason))
}
