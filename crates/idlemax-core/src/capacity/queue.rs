//! Domain: what the queued jobs do on a device that is online, not held and has room.
//! Eligibility is the scheduler's (`device_blockers`, `job_blockers_in`, `pick`); this only
//! counts and words it.

use std::collections::BTreeMap;

use super::{CapacityInput, CapacityState};
use crate::control::paused_accounts;
use crate::explain::ineligible_causes;
use crate::scheduler::{
    ai_leased_accounts, device_blockers, job_blockers_in, pick, Blocker, SchedContext,
};
use crate::types::{is_dispatcher_job, Job, JobStatus};

/// What the device may still start.
pub(super) struct Room {
    /// Jobs it may hold at once.
    pub slots: u32,
    pub free_slots: u32,
    pub free_mem_mib: u64,
}

pub(super) struct Verdict {
    pub state: CapacityState,
    pub why: String,
}

fn verdict(state: CapacityState, why: impl Into<String>) -> Verdict {
    Verdict {
        state,
        why: why.into(),
    }
}

/// "1 queued job", "3 queued jobs".
fn queued_jobs(n: usize) -> String {
    format!("{n} queued {}", if n == 1 { "job" } else { "jobs" })
}

/// A queued job and the first thing that holds it on every device alike, if anything.
struct Waiting<'a> {
    job: &'a Job,
    wait: Option<Blocker>,
}

/// The queued jobs, with what holds each on every device alike worked out once for all devices.
pub(super) struct Queue<'a> {
    jobs: Vec<Waiting<'a>>,
    paused_accounts: Vec<String>,
}

impl<'a> Queue<'a> {
    pub(super) fn new(input: &CapacityInput<'a>) -> Self {
        let paused_accounts = paused_accounts(input.paused);
        let leased = ai_leased_accounts(input.jobs);
        let jobs = input
            .jobs
            .iter()
            .filter(|j| j.status == JobStatus::Queued && !is_dispatcher_job(&j.spec))
            .map(|job| {
                let wait =
                    job_blockers_in(input.now_ms, input.quota, &paused_accounts, &leased, job);
                Waiting {
                    job,
                    wait: wait.into_iter().next(),
                }
            })
            .collect();
        Self {
            jobs,
            paused_accounts,
        }
    }

    pub(super) fn paused_accounts(&self) -> &[String] {
        &self.paused_accounts
    }

    pub(super) fn judge(
        &self,
        input: &CapacityInput<'_>,
        ctx: &SchedContext<'_>,
        room: &Room,
    ) -> Verdict {
        let ready: Vec<&Job> = self
            .jobs
            .iter()
            .filter(|w| w.wait.is_none() && device_blockers(ctx.device, w.job).is_empty())
            .map(|w| w.job)
            .collect();
        if room.free_slots == 0 || room.free_mem_mib == 0 {
            return full(room, ready.len());
        }
        let picked = pick(ctx, input.jobs);
        if !picked.is_empty() {
            let starting = queued_jobs(picked.len());
            return verdict(
                CapacityState::Starting,
                format!("starting {starting} on the next heartbeat"),
            );
        }
        // Ready jobs and room, yet `pick` takes none: every ready job needs more RAM.
        if let Some(need) = ready.iter().map(|j| j.spec.mem_mb).min() {
            return verdict(
                CapacityState::TooBig,
                format!(
                    "the smallest queued job needs {need} MiB but only {} MiB of the RAM budget is free",
                    room.free_mem_mib
                ),
            );
        }
        self.unready(input, ctx)
    }

    /// Room to spare and nothing ready: the cause most queued jobs share, or an empty queue.
    /// A cause that needs a change (a missing tool, a job limited to other devices) wins a tie
    /// over one that clears by waiting.
    fn unready(&self, input: &CapacityInput<'_>, ctx: &SchedContext<'_>) -> Verdict {
        let mut causes: BTreeMap<(bool, String), usize> = BTreeMap::new();
        for w in &self.jobs {
            let fixed = device_blockers(ctx.device, w.job);
            let cause = if fixed.is_empty() {
                w.wait.as_ref().map(|b| (false, wait_phrase(b)))
            } else {
                ineligible_causes(&fixed, w.job)
                    .into_iter()
                    .next()
                    .map(|c| (true, c))
            };
            if let Some(cause) = cause {
                *causes.entry(cause).or_default() += 1;
            }
        }
        let top = causes.into_iter().max_by(|a, b| {
            a.1.cmp(&b.1)
                .then(a.0 .0.cmp(&b.0 .0))
                .then(b.0 .1.cmp(&a.0 .1))
        });
        match top {
            Some(((true, cause), n)) => verdict(
                CapacityState::CannotRun,
                format!("{} cannot run here: {cause}", queued_jobs(n)),
            ),
            Some(((false, cause), n)) => verdict(
                CapacityState::Waiting,
                format!("{} waiting: {cause}", queued_jobs(n)),
            ),
            None => paused_or_empty(input, ctx),
        }
    }
}

fn full(room: &Room, waiting: usize) -> Verdict {
    let mut why = if room.free_slots == 0 {
        let noun = if room.slots == 1 { "slot" } else { "slots" };
        format!("all {} {noun} in use", room.slots)
    } else {
        "the RAM budget is fully used".to_string()
    };
    if waiting > 0 {
        why.push_str(&format!(", {} waiting", queued_jobs(waiting)));
    }
    verdict(CapacityState::Full, why)
}

/// Nothing queued can start here: held jobs this device could run say so, else the queue is empty.
fn paused_or_empty(input: &CapacityInput<'_>, ctx: &SchedContext<'_>) -> Verdict {
    let held = input
        .held
        .iter()
        .filter(|j| device_blockers(ctx.device, j).is_empty())
        .count();
    if held > 0 {
        return verdict(
            CapacityState::Waiting,
            format!("{} paused by you", queued_jobs(held)),
        );
    }
    verdict(CapacityState::Idle, "queue empty")
}

/// A job-wide blocker in a few words.
fn wait_phrase(b: &Blocker) -> String {
    match b {
        Blocker::BackingOff { .. } => "retry delay".into(),
        Blocker::QuotaShort { .. } => "AI quota".into(),
        Blocker::AccountLeased(_) => "its AI account is busy".into(),
        Blocker::AccountPaused(_) => "its AI account is paused".into(),
        Blocker::NoAiAccount => "no AI account set".into(),
        other => other.to_string(),
    }
}
