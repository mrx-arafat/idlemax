//! Agent tick decisions: which jobs to start, return, pause, resume or kill. Domain layer, pure.
//!
//! The agent adapter feeds each tick's guard verdict, kill switch, coordinator commands,
//! local controls and new leases into [`AgentState::plan`], runs the returned actions, and
//! reports each outcome back through [`AgentState::applied`].

use std::collections::{BTreeMap, BTreeSet};

mod journal;
mod outbox;

pub use journal::{encode_ack, encode_update, replay, Replayed};
pub use outbox::Outbox;

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
/// coordinator treats as final (no retry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillOrder {
    pub reason: String,
    pub owner_stop: bool,
}

impl KillOrder {
    fn new(reason: impl Into<String>, owner_stop: bool) -> Self {
        Self {
            reason: reason.into(),
            owner_stop,
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
}

impl AgentState {
    /// Paused by a local control (not the kill switch file).
    pub fn local_paused(&self) -> bool {
        self.local_paused
    }

    pub fn jobs(&self) -> impl Iterator<Item = &TrackedJob> {
        self.jobs.values()
    }

    /// Updates holds and returns this tick's actions. Kills come first, starts last.
    pub fn plan(&mut self, input: &TickInput<'_>) -> Vec<AgentAction> {
        let mut kills = BTreeMap::new();
        self.apply_local_controls(input.local_controls, &mut kills);
        let stop_all = self.apply_commands(input.commands, &mut kills);
        if input.shutting_down {
            self.kill_all(&KillOrder::new(SHUTDOWN_REASON, false), &mut kills);
        }
        let local = input.kill_switch || self.local_paused;
        for job in self.jobs.values_mut() {
            job.holds.local = local;
            match job_verdict(input.guard, job.on_user_active) {
                GuardAction::Kill => {
                    kills
                        .entry(job.id)
                        .or_insert_with(|| KillOrder::new(guard_reason(input.guard), false));
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
                Some(update(job.id, JobStatus::Running, None))
            }
            (AgentAction::Start(job), Err(e)) => Some(update(
                job.id,
                JobStatus::Failed,
                Some(format!("start: {e}")),
            )),
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
    /// ordered gets its reason and owner-stop flag.
    pub fn finished(&mut self, mut update: JobUpdate) -> JobUpdate {
        if !update.status.is_terminal() {
            return update;
        }
        let order = self.jobs.remove(&update.id).and_then(|j| j.kill);
        if let (JobStatus::Killed, Some(order)) = (update.status, order) {
            update.owner_stop = order.owner_stop;
            if update.error.is_none() {
                update.error = Some(order.reason);
            }
        }
        update
    }

    fn set_paused(&mut self, id: JobId, paused: bool) -> bool {
        self.jobs.get_mut(&id).map(|j| j.paused = paused).is_some()
    }

    fn apply_local_controls(&mut self, controls: &[ControlAction], kills: &mut Kills) {
        for c in controls {
            match c {
                ControlAction::Pause => self.local_paused = true,
                ControlAction::Resume => self.local_paused = false,
                ControlAction::StopAll => {
                    self.local_paused = true;
                    self.kill_all(&KillOrder::new("stop_all from local control", true), kills);
                }
                // Not a Global-scope action; the local API rejects it.
                ControlAction::Cancel => {}
            }
        }
    }

    /// Returns true when the coordinator sent StopAll.
    fn apply_commands(&mut self, commands: &[AgentCommand], kills: &mut Kills) -> bool {
        let mut stop_all = false;
        for c in commands {
            match c {
                AgentCommand::PauseJob { id } => self.set_command_hold(*id, true),
                AgentCommand::ResumeJob { id } => self.set_command_hold(*id, false),
                AgentCommand::KillJob { id } if self.jobs.contains_key(id) => {
                    kills
                        .entry(*id)
                        .or_insert_with(|| KillOrder::new("killed by coordinator", false));
                }
                AgentCommand::KillJob { .. } => {}
                AgentCommand::StopAll => {
                    stop_all = true;
                    let order = KillOrder::new("stop_all from coordinator", false);
                    self.kill_all(&order, kills);
                }
                AgentCommand::ResumeAll => {
                    self.jobs.values_mut().for_each(|j| j.holds.command = false);
                }
            }
        }
        stop_all
    }

    fn set_command_hold(&mut self, id: JobId, hold: bool) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.holds.command = hold;
        }
    }

    fn kill_all(&self, order: &KillOrder, kills: &mut Kills) {
        for id in self.jobs.keys() {
            kills.entry(*id).or_insert_with(|| order.clone());
        }
    }

    /// Starts leases while slots and RAM remain; returns the rest.
    fn plan_starts(&self, input: &TickInput<'_>, blocked: Option<String>) -> Vec<AgentAction> {
        let (mut slots, mut mem_mib) = self.budget_left(input.guard);
        let mut seen = BTreeSet::new();
        let mut actions = Vec::new();
        for job in input.assign {
            // Already running here or listed twice: never start or return it twice.
            if self.jobs.contains_key(&job.id) || !seen.insert(job.id) {
                continue;
            }
            let reason = match &blocked {
                Some(r) => Some(r.clone()),
                None if slots == 0 => Some("no free job slot".to_string()),
                None if job.spec.mem_mb > mem_mib => Some(format!(
                    "needs {} MiB, guard allows {mem_mib} MiB",
                    job.spec.mem_mb
                )),
                None => None,
            };
            match reason {
                Some(reason) => actions.push(AgentAction::Return { id: job.id, reason }),
                None => {
                    slots -= 1;
                    mem_mib -= job.spec.mem_mb;
                    actions.push(AgentAction::Start(Box::new(job.clone())));
                }
            }
        }
        actions
    }

    /// Guard slots and RAM minus jobs started since the guard sampled.
    fn budget_left(&self, guard: &GuardDecision) -> (u32, u64) {
        let GuardAction::Run { slots, mem_mib } = guard.action else {
            return (0, 0);
        };
        let since: Vec<&TrackedJob> = self
            .jobs
            .values()
            .filter(|j| j.started_ms >= guard.at_ms)
            .collect();
        let used_mem: u64 = since.iter().map(|j| j.mem_mb).sum();
        let used_slots = u32::try_from(since.len()).unwrap_or(u32::MAX);
        (
            slots.saturating_sub(used_slots),
            mem_mib.saturating_sub(used_mem),
        )
    }
}

type Kills = BTreeMap<JobId, KillOrder>;

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
        action: GuardAction::Pause,
        resource_action: GuardAction::Pause,
        user_active: false,
        reasons: vec![reason.to_string()],
        at_ms,
    }
}

fn start_blocker(input: &TickInput<'_>, local: bool, stop_all: bool) -> Option<String> {
    let reason = if input.shutting_down {
        SHUTDOWN_REASON
    } else if !input.online {
        "coordinator unreachable"
    } else if input.kill_switch {
        "kill switch file present"
    } else if local {
        "paused locally"
    } else if stop_all {
        "stop_all from coordinator"
    } else if !input.guard.action.may_start() {
        return Some(guard_reason(input.guard));
    } else {
        return None;
    };
    Some(reason.to_string())
}

fn guard_reason(guard: &GuardDecision) -> String {
    if guard.reasons.is_empty() {
        format!("guard: {:?}", guard.action)
    } else {
        format!("guard: {}", guard.reasons.join("; "))
    }
}

fn tracked(job: &Job, now_ms: UnixMs) -> TrackedJob {
    TrackedJob {
        id: job.id,
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
