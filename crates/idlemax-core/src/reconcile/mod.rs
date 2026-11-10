//! Agent tick decisions: which jobs to start, return, pause, resume or kill. Domain layer, pure.
//!
//! The agent adapter feeds each tick's guard verdict, kill switch, coordinator commands,
//! local controls and new leases into [`AgentState::plan`], runs the returned actions, and
//! reports each outcome back through [`AgentState::applied`].
//!
//! This file holds the state types and the tick API (`plan`, `applied`, `finished`). The
//! children hold what `plan` calls: `controls` (local controls and coordinator commands)
//! and `starts` (which leases fit, and why one is handed back).

use std::collections::{BTreeMap, BTreeSet};

mod controls;
mod journal;
mod outbox;
mod starts;

pub use journal::{encode_ack, encode_update, replay, Replayed};
pub use outbox::Outbox;

use self::starts::{guard_reason, start_blocker};

use crate::protocol::{AgentCommand, JobUpdate};
use crate::types::{
    ActivePolicy, ControlAction, DeviceId, GuardAction, GuardDecision, Job, JobId, JobStatus,
    UnixMs,
};

/// Why a job is held paused. It runs only while no hold is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Holds {
    pub guard: bool,
    /// Coordinator PauseJob.
    pub command: bool,
    /// Kill switch file or local pause.
    pub local: bool,
}

impl Holds {
    pub fn any(self) -> bool {
        self.guard || self.command || self.local
    }
}

/// A job this device runs, as the agent sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedJob {
    pub id: JobId,
    pub name: String,
    pub mem_mb: u64,
    pub on_user_active: ActivePolicy,
    pub started_ms: UnixMs,
    /// SIGSTOPped right now.
    pub paused: bool,
    pub holds: Holds,
    /// Kill sent; the runner reports the exit.
    pub kill: Option<KillOrder>,
}

/// Why a job is killed. `owner_stop` marks the owner's local stop-all, which the
/// coordinator treats as final. `retryable` marks a kill that is not the job's fault (the
/// guard, an agent shutting down), after which the coordinator may run the job again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KillOrder {
    pub reason: String,
    pub owner_stop: bool,
    pub retryable: bool,
}

impl KillOrder {
    /// A final kill: the owner stopped everything, or the coordinator ordered it.
    fn new(reason: impl Into<String>, owner_stop: bool) -> Self {
        Self {
            reason: reason.into(),
            owner_stop,
            retryable: false,
        }
    }

    /// A kill the job did not cause, so the job may run again.
    fn retryable(reason: impl Into<String>) -> Self {
        Self {
            retryable: true,
            ..Self::new(reason, false)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentAction {
    Start(Box<Job>),
    /// Give the lease back without starting the job.
    Return {
        id: JobId,
        reason: String,
    },
    Pause(JobId),
    Resume(JobId),
    Kill {
        id: JobId,
        order: KillOrder,
    },
}

impl AgentAction {
    pub fn job_id(&self) -> JobId {
        match self {
            AgentAction::Start(job) => job.id,
            AgentAction::Return { id, .. }
            | AgentAction::Pause(id)
            | AgentAction::Resume(id)
            | AgentAction::Kill { id, .. } => *id,
        }
    }
}

/// Everything one tick decides on.
#[derive(Debug, Clone, Copy)]
pub struct TickInput<'a> {
    pub guard: &'a GuardDecision,
    /// `$IDLEMAX_HOME/PAUSE` exists.
    pub kill_switch: bool,
    /// The last heartbeat reached the coordinator.
    pub online: bool,
    pub commands: &'a [AgentCommand],
    pub local_controls: &'a [ControlAction],
    /// New leases from the heartbeat reply.
    pub assign: &'a [Job],
    /// The agent is exiting: kill everything, start nothing. Not an owner stop, so the
    /// coordinator retries the jobs after a restart.
    pub shutting_down: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AgentState {
    jobs: BTreeMap<JobId, TrackedJob>,
    local_paused: bool,
    sampled_jobs: BTreeSet<JobId>,
    sample_at_ms: Option<UnixMs>,
}

impl AgentState {
    /// Paused by a local control (not the kill switch file).
    pub fn local_paused(&self) -> bool {
        self.local_paused
    }

    pub fn jobs(&self) -> impl Iterator<Item = &TrackedJob> {
        self.jobs.values()
    }

    /// Looks up the spec name of a tracked job.
    pub fn job_name(&self, id: JobId) -> Option<&str> {
        self.jobs.get(&id).map(|j| j.name.as_str())
    }

    /// Records which jobs a fresh guard budget already accounts for, even after clock steps.
    pub fn sampled(&mut self, at_ms: UnixMs) {
        self.sampled_jobs = self.jobs.keys().copied().collect();
        self.sample_at_ms = Some(at_ms);
    }

    /// Updates holds and returns this tick's actions. Kills come first, starts last.
    pub fn plan(&mut self, input: &TickInput<'_>) -> Vec<AgentAction> {
        if self.sample_at_ms != Some(input.guard.at_ms) {
            self.sampled(input.guard.at_ms);
        }
        let mut kills = BTreeMap::new();
        self.apply_local_controls(input.local_controls, &mut kills);
        let stop_all = self.apply_commands(input.commands, &mut kills);
        if input.shutting_down {
            self.kill_all(&KillOrder::retryable(SHUTDOWN_REASON), &mut kills);
        }
        let local = input.kill_switch || self.local_paused;
        for job in self.jobs.values_mut() {
            job.holds.local = local;
            match job_verdict(input.guard, job.on_user_active) {
                GuardAction::Kill => {
                    kills
                        .entry(job.id)
                        .or_insert_with(|| KillOrder::retryable(guard_reason(input.guard)));
                }
                GuardAction::Pause => job.holds.guard = true,
                GuardAction::Run { .. } => job.holds.guard = false,
                GuardAction::Hold => {}
            }
        }
        let mut actions = Vec::new();
        for job in self.jobs.values().filter(|j| j.kill.is_none()) {
            if let Some(order) = kills.remove(&job.id) {
                actions.push(AgentAction::Kill { id: job.id, order });
            } else if job.holds.any() && !job.paused {
                actions.push(AgentAction::Pause(job.id));
            } else if !job.holds.any() && job.paused {
                actions.push(AgentAction::Resume(job.id));
            }
        }
        let blocked = start_blocker(input, local, stop_all);
        actions.extend(self.plan_starts(input, blocked));
        actions
    }

    /// Records an action's outcome and returns the job update to report, if any.
    pub fn applied(
        &mut self,
        action: &AgentAction,
        outcome: Result<(), String>,
        device: &DeviceId,
        now_ms: UnixMs,
    ) -> Option<JobUpdate> {
        let update = |id, status, error| JobUpdate {
            id,
            device: device.clone(),
            status,
            at_ms: now_ms,
            error,
            ..JobUpdate::default()
        };
        match (action, outcome) {
            (AgentAction::Start(job), Ok(())) => {
                self.jobs.insert(job.id, tracked(job, now_ms));
                Some(JobUpdate {
                    attempt: job.attempts,
                    ..update(job.id, JobStatus::Running, None)
                })
            }
            // The agent could not start it (not allowed here, no such binary): another
            // device or a later try may.
            (AgentAction::Start(job), Err(e)) => Some(JobUpdate {
                attempt: job.attempts,
                retryable: true,
                ..update(job.id, JobStatus::Failed, Some(format!("start: {e}")))
            }),
            (AgentAction::Return { id, reason }, _) => {
                Some(update(*id, JobStatus::Queued, Some(reason.clone())))
            }
            (AgentAction::Pause(id), Ok(())) => self
                .set_paused(*id, true)
                .then(|| update(*id, JobStatus::Paused, None)),
            (AgentAction::Resume(id), Ok(())) => self
                .set_paused(*id, false)
                .then(|| update(*id, JobStatus::Running, None)),
            (AgentAction::Kill { id, order }, Ok(())) => {
                if let Some(job) = self.jobs.get_mut(id) {
                    job.kill = Some(order.clone());
                }
                None
            }
            // Failed pause, resume or kill: state unchanged, so the next tick retries.
            (_, Err(_)) => None,
        }
    }

    /// Takes a runner update; a terminal one stops tracking the job, and a kill the agent
    /// ordered gets its reason and whether it was an owner stop or is retryable. A runner
    /// that already named a reason (memory cap, max runtime) stopped the job for its own
    /// limits, which stays final even if a guard kill was pending.
    pub fn finished(&mut self, mut update: JobUpdate) -> JobUpdate {
        if !update.status.is_terminal() {
            return update;
        }
        self.sampled_jobs.remove(&update.id);
        let order = self.jobs.remove(&update.id).and_then(|j| j.kill);
        if let (JobStatus::Killed, Some(order)) = (update.status, order) {
            update.owner_stop = order.owner_stop;
            if update.error.is_none() {
                update.error = Some(order.reason);
                update.retryable = order.retryable;
            }
        }
        update
    }

    fn set_paused(&mut self, id: JobId, paused: bool) -> bool {
        self.jobs.get_mut(&id).map(|j| j.paused = paused).is_some()
    }
}

const SHUTDOWN_REASON: &str = "agent shutting down";

/// The guard verdict that applies to one job, by its on_user_active policy.
pub fn job_verdict(guard: &GuardDecision, policy: ActivePolicy) -> GuardAction {
    match policy {
        ActivePolicy::Pause => guard.action,
        ActivePolicy::Continue => guard.resource_action,
        ActivePolicy::Kill if guard.user_active => GuardAction::Kill,
        ActivePolicy::Kill => guard.resource_action,
    }
}

/// Verdict when device health is unknown (a sample failed): pause everything.
pub fn fail_safe_decision(reason: &str, at_ms: UnixMs) -> GuardDecision {
    GuardDecision {
        memory_limit: None,
        action: GuardAction::Pause,
        resource_action: GuardAction::Pause,
        user_active: false,
        reasons: vec![reason.to_string()],
        at_ms,
    }
}

fn tracked(job: &Job, now_ms: UnixMs) -> TrackedJob {
    TrackedJob {
        id: job.id,
        name: job.spec.name.clone(),
        mem_mb: job.spec.mem_mb,
        on_user_active: job.spec.on_user_active,
        started_ms: now_ms,
        paused: false,
        holds: Holds::default(),
        kill: None,
    }
}

#[cfg(test)]
mod tests;
