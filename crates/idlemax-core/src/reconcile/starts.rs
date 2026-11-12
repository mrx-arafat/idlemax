//! Starting leases: how many fit the guard's slots and RAM, and why a lease is handed back.
//! Part of the agent tick decisions (domain layer, pure).

use std::collections::BTreeSet;

use super::workers::worker_blocker;
use super::{AgentAction, AgentState, TickInput, TrackedJob, SHUTDOWN_REASON};
use crate::types::{is_dispatcher_job, Admission, GuardAction, GuardDecision};

impl AgentState {
    /// Starts leases while slots and RAM remain; returns the rest. A worker lease follows the
    /// line state alone (`worker_blocker`), checked first. A dispatcher lease skips
    /// the guard and its budget: it starts when no hard blocker is set, the lines allow a
    /// start and no other dispatcher job runs here (A16).
    pub(super) fn plan_starts(
        &self,
        input: &TickInput<'_>,
        hard: Option<String>,
    ) -> Vec<AgentAction> {
        let blocked = hard.clone().or_else(|| guard_blocker(input.guard));
        let (mut slots, mut mem_mib) = self.budget_left(input.guard);
        let mut lines_busy = self.jobs.values().any(|j| j.admission == Admission::Lines);
        let mut seen = BTreeSet::new();
        let mut actions = Vec::new();
        for job in input.assign {
            // Already running here or listed twice: never start or return it twice.
            if self.jobs.contains_key(&job.id) || !seen.insert(job.id) {
                continue;
            }
            let reason = if let Some(w) = &job.spec.worker {
                worker_blocker(input, &hard, w.action)
            } else if is_dispatcher_job(&job.spec) {
                let reason = lines_blocker(input.lines, &hard, lines_busy);
                lines_busy |= reason.is_none();
                reason
            } else {
                match &blocked {
                    Some(r) => Some(r.clone()),
                    None if slots == 0 => Some("no free job slot".to_string()),
                    None if job.spec.mem_mb > mem_mib => Some(format!(
                        "needs {} MiB, guard allows {mem_mib} MiB",
                        job.spec.mem_mb
                    )),
                    None => {
                        slots -= 1;
                        mem_mib -= job.spec.mem_mb;
                        None
                    }
                }
            };
            actions.push(match reason {
                Some(reason) => AgentAction::Return { id: job.id, reason },
                None => AgentAction::Start(Box::new(job.clone())),
            });
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

/// What holds every start whatever the guard or the lines say: shutdown, no coordinator,
/// the kill switch, a local pause and `stop_all`.
pub(super) fn hard_blocker(input: &TickInput<'_>, local: bool, stop_all: bool) -> Option<String> {
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
    } else {
        return None;
    };
    Some(reason.to_string())
}

/// The guard part of a plain job's start check.
fn guard_blocker(guard: &GuardDecision) -> Option<String> {
    (!guard.action.may_start()).then(|| guard_reason(guard))
}

fn lines_blocker(lines: &GuardDecision, hard: &Option<String>, busy: bool) -> Option<String> {
    if let Some(r) = hard {
        return Some(r.clone());
    }
    if !lines.action.may_start() {
        return Some(verdict_reason(lines));
    }
    busy.then(|| "another dispatcher job runs on this device".to_string())
}

/// A lines verdict carries its own `lines:` prefix; a guard verdict gets `guard:`.
pub(super) fn verdict_reason(v: &GuardDecision) -> String {
    if v.reasons.first().is_some_and(|r| r.starts_with("lines:")) {
        v.reasons.join("; ")
    } else {
        guard_reason(v)
    }
}

pub(super) fn guard_reason(guard: &GuardDecision) -> String {
    if guard.reasons.is_empty() {
        format!("guard: {:?}", guard.action)
    } else {
        format!("guard: {}", guard.reasons.join("; "))
    }
}
