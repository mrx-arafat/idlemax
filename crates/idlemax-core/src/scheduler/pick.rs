//! Domain: which queued jobs a device starts now, in start order, within its guard slots and
//! memory budget. Pure: no I/O, time is a parameter.

use std::collections::BTreeSet;

use super::blockers::{
    ai_leased_accounts, device_blockers, job_blockers_in, token_class_pct, SchedContext,
};
use crate::types::{CostClass, GuardAction, Job, JobId, JobStatus};

/// Slots and MiB left after jobs leased to this device that the agent has not started yet.
/// None unless the guard lets new jobs start.
pub fn start_budget(ctx: &SchedContext<'_>, jobs: &[Job]) -> Option<(u32, u64)> {
    let GuardAction::Run { slots, mem_mib } = ctx.guard.action else {
        return None;
    };
    let pending = jobs.iter().filter(|j| {
        j.status == JobStatus::Leased
            && j.device.as_deref() == Some(ctx.device.id.as_str())
            && !ctx.running.iter().any(|r| r.id == j.id)
    });
    let (n, mem) = pending.fold((0u32, 0u64), |(n, m), j| (n + 1, m + j.spec.mem_mb));
    Some((slots.saturating_sub(n), mem_mib.saturating_sub(mem)))
}

/// Queued job ids to lease to this device now, in start order.
pub fn pick(ctx: &SchedContext<'_>, jobs: &[Job]) -> Vec<JobId> {
    if ctx.paused {
        return Vec::new();
    }
    let Some((slots, mut mem_left)) = start_budget(ctx, jobs) else {
        return Vec::new();
    };
    let leased = ai_leased_accounts(jobs);
    let mut cands: Vec<&Job> = jobs
        .iter()
        .filter(|j| {
            j.status == JobStatus::Queued
                && job_blockers_in(ctx.now_ms, ctx.quota, ctx.paused_accounts, &leased, j)
                    .is_empty()
                && device_blockers(ctx.device, j).is_empty()
        })
        .collect();
    cands.sort_by(|a, b| start_order(a, b));
    let mut picked = Vec::new();
    let mut accounts = BTreeSet::new();
    for job in cands {
        if picked.len() >= slots as usize {
            break;
        }
        if job.spec.mem_mb > mem_left {
            continue;
        }
        // One AI job per account, also within one batch.
        if let Some(account) = job.spec.ai_account.as_deref() {
            if !accounts.insert(account) {
                continue;
            }
        }
        mem_left -= job.spec.mem_mb;
        picked.push(job.id);
    }
    picked
}

/// Priority first, then the largest AI job that fits, then oldest.
fn start_order(a: &Job, b: &Job) -> std::cmp::Ordering {
    b.spec
        .priority
        .cmp(&a.spec.priority)
        .then(ai_size(b).total_cmp(&ai_size(a)))
        .then(a.created_ms.cmp(&b.created_ms))
        .then(a.id.cmp(&b.id))
}

fn ai_size(job: &Job) -> f64 {
    match job.spec.cost {
        CostClass::Ai => token_class_pct(job.spec.token_class),
        _ => 0.0,
    }
}
