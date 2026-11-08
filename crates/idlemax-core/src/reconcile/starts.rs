//! Starting leases: how many fit the guard's slots and RAM, and why a lease is handed back.
//! Part of the agent tick decisions (domain layer, pure).

use std::collections::BTreeSet;

use super::{AgentAction, AgentState, TickInput, TrackedJob, SHUTDOWN_REASON};
use crate::types::{GuardAction, GuardDecision};

impl AgentState {
    /// Starts leases while slots and RAM remain; returns the rest.
    pub(super) fn plan_starts(
        &self,
        input: &TickInput<'_>,
        blocked: Option<String>,
    ) -> Vec<AgentAction> {
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
            .filter(|j| !self.sampled_jobs.contains(&j.id))
            .collect();
        let used_mem: u64 = since.iter().map(|j| j.mem_mb).sum();
        let used_slots = u32::try_from(since.len()).unwrap_or(u32::MAX);
        (
            slots.saturating_sub(used_slots),
            mem_mib.saturating_sub(used_mem),
        )
    }
}

pub(super) fn start_blocker(input: &TickInput<'_>, local: bool, stop_all: bool) -> Option<String> {
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

pub(super) fn guard_reason(guard: &GuardDecision) -> String {
    if guard.reasons.is_empty() {
        format!("guard: {:?}", guard.action)
    } else {
        format!("guard: {}", guard.reasons.join("; "))
    }
}
