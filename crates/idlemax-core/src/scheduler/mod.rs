//! Domain: which queued jobs a device may lease now, lease deadlines, and the
//! retry / park policy. Pure: no I/O, time is a parameter.
//!
//! Caller contract (coord api + store):
//! - `jobs` is every non-terminal job in the cluster (queued and active).
//! - `attempts` counts leases: increment it when a job is leased.
//! - Set `lease_expires_ms = lease_deadline(job, now)` on lease and renew it the same
//!   way on every heartbeat that lists the job, so a paused job keeps its lease while
//!   its agent is alive and the runner's own timeout fires before an orphaned lease ends.
//! - Persist what `expire_leases` and `apply_update` return as the job's new state.

use std::collections::BTreeSet;
use std::fmt;

use crate::protocol::{JobUpdate, RunningJob};
use crate::quota::QuotaView;
use crate::types::{
    CostClass, DeviceInfo, DeviceProfile, GuardAction, GuardDecision, Job, JobId, JobStatus,
    TokenClass, UnixMs,
};

/// Slack on top of max runtime so the runner's timeout fires before the lease ends.
pub const LEASE_GRACE_MS: i64 = 120_000;
pub const BACKOFF_BASE_MS: i64 = 120_000;
pub const BACKOFF_MAX_MS: i64 = 3_600_000;

/// One device asking for work on a heartbeat.
#[derive(Debug, Clone, Copy)]
pub struct SchedContext<'a> {
    pub now_ms: UnixMs,
    pub device: &'a DeviceInfo,
    pub guard: &'a GuardDecision,
    /// Jobs the agent reports running or paused.
    pub running: &'a [RunningJob],
    /// Quota views from `quota::views`; an AI account needs every window spendable.
    pub quota: &'a [QuotaView],
    /// Global pause, a device control pause, or the device's own local pause.
    pub paused: bool,
    pub paused_accounts: &'a [String],
}

/// Why a job may not be leased to this device now.
#[derive(Debug, Clone, PartialEq)]
pub enum Blocker {
    NotQueued(JobStatus),
    BackingOff {
        until_ms: UnixMs,
    },
    DeviceNotAllowed,
    MissingCapability(String),
    CostNotAllowed(CostClass),
    NoAiAccount,
    AccountNotOnDevice(String),
    AccountPaused(String),
    QuotaShort {
        account: String,
        headroom_pct: f64,
        needs_pct: f64,
    },
    AccountLeased(String),
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocker::NotQueued(s) => write!(f, "status is {s}"),
            Blocker::BackingOff { until_ms } => write!(f, "backing off until {until_ms}"),
            Blocker::DeviceNotAllowed => f.write_str("device not in the job's device list"),
            Blocker::MissingCapability(c) => write!(f, "device lacks {c}"),
            Blocker::CostNotAllowed(c) => write!(f, "cost class {c:?} not allowed on this profile"),
            Blocker::NoAiAccount => f.write_str("ai job without ai_account"),
            Blocker::AccountNotOnDevice(a) => write!(f, "account {a} not allowed on this device"),
            Blocker::AccountPaused(a) => write!(f, "account {a} is paused"),
            Blocker::QuotaShort {
                account,
                headroom_pct,
                needs_pct,
            } => write!(
                f,
                "account {account} has {headroom_pct:.0}% spendable, job needs {needs_pct:.0}%"
            ),
            Blocker::AccountLeased(a) => write!(f, "account {a} already has an AI job leased"),
        }
    }
}

/// Rough share of one window a job of this class may burn, in percent.
// SHORTCUT: fixed estimates, learn per-job usage from the quota ledger once it holds a week of runs.
pub fn token_class_pct(class: Option<TokenClass>) -> f64 {
    match class {
        Some(TokenClass::Small) | None => 3.0,
        Some(TokenClass::Medium) => 8.0,
        Some(TokenClass::Large) => 20.0,
    }
}

/// Servers take only CpuLight and Io jobs.
pub fn cost_allowed(profile: DeviceProfile, cost: CostClass) -> bool {
    match profile {
        DeviceProfile::MacosLaptop => true,
        DeviceProfile::LinuxServer => matches!(cost, CostClass::CpuLight | CostClass::Io),
    }
}

/// Spendable headroom for an account: the smallest window headroom, or 0 when any
/// window is not spendable or no window is known.
pub fn spendable_headroom(quota: &[QuotaView], account: &str) -> f64 {
    let mut views = quota.iter().filter(|v| v.account == account).peekable();
    if views.peek().is_none() {
        return 0.0;
    }
    views.fold(f64::INFINITY, |acc, v| {
        if v.spendable {
            acc.min(v.headroom_pct)
        } else {
            0.0
        }
    })
}

/// Accounts with an AI job holding a lease anywhere in the cluster.
pub fn ai_leased_accounts(jobs: &[Job]) -> BTreeSet<&str> {
    jobs.iter()
        .filter(|j| j.status.is_active() && j.spec.cost == CostClass::Ai)
        .filter_map(|j| j.spec.ai_account.as_deref())
        .collect()
}

/// Why `job` may not be leased to `ctx.device` now; None when it may.
pub fn blocker(ctx: &SchedContext<'_>, jobs: &[Job], job: &Job) -> Option<Blocker> {
    let dev = ctx.device;
    let spec = &job.spec;
    if job.status != JobStatus::Queued {
        return Some(Blocker::NotQueued(job.status));
    }
    if let Some(until_ms) = job.not_before_ms.filter(|t| *t > ctx.now_ms) {
        return Some(Blocker::BackingOff { until_ms });
    }
    if !spec.devices.is_empty() && !spec.devices.contains(&dev.id) {
        return Some(Blocker::DeviceNotAllowed);
    }
    if let Some(miss) = spec.requires.iter().find(|r| !dev.capabilities.contains(r)) {
        return Some(Blocker::MissingCapability(miss.clone()));
    }
    if !cost_allowed(dev.profile, spec.cost) {
        return Some(Blocker::CostNotAllowed(spec.cost));
    }
    if spec.cost == CostClass::Ai {
        return ai_blocker(ctx, jobs, job);
    }
    None
}

fn ai_blocker(ctx: &SchedContext<'_>, jobs: &[Job], job: &Job) -> Option<Blocker> {
    let Some(account) = job.spec.ai_account.as_deref() else {
        return Some(Blocker::NoAiAccount);
    };
    let owned = || account.to_string();
    if !ctx.device.ai_accounts.iter().any(|a| a == account) {
        return Some(Blocker::AccountNotOnDevice(owned()));
    }
    if ctx.paused_accounts.iter().any(|a| a == account) {
        return Some(Blocker::AccountPaused(owned()));
    }
    let headroom_pct = spendable_headroom(ctx.quota, account);
    let needs_pct = token_class_pct(job.spec.token_class);
    if headroom_pct < needs_pct {
        return Some(Blocker::QuotaShort {
            account: owned(),
            headroom_pct,
            needs_pct,
        });
    }
    if ai_leased_accounts(jobs).contains(account) {
        return Some(Blocker::AccountLeased(owned()));
    }
    None
}

/// Slots and MiB left after jobs leased to this device that the agent has not started yet.
fn start_budget(ctx: &SchedContext<'_>, jobs: &[Job]) -> Option<(u32, u64)> {
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
    let mut cands: Vec<&Job> = jobs
        .iter()
        .filter(|j| blocker(ctx, jobs, j).is_none())
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

pub fn lease_deadline(job: &Job, now_ms: UnixMs) -> UnixMs {
    let runtime_ms =
        i64::try_from(job.spec.max_runtime_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
    now_ms
        .saturating_add(runtime_ms)
        .saturating_add(LEASE_GRACE_MS)
}

/// Delay before the next attempt after `attempts` leases: 2, 4, 8 ... min, capped at 1 h.
pub fn backoff_ms(attempts: u32) -> i64 {
    let exp = attempts.saturating_sub(1).min(16);
    (BACKOFF_BASE_MS << exp).min(BACKOFF_MAX_MS)
}

/// The job after a failure, a guard kill or a lost lease: back to Queued with backoff,
/// or Parked after `effective_max_attempts` leases. AI jobs always park.
pub fn retry_or_park(job: &Job, now_ms: UnixMs, cause: &str) -> Job {
    let max = job.spec.effective_max_attempts();
    let mut next = Job {
        device: None,
        lease_expires_ms: None,
        ..job.clone()
    };
    if job.spec.cost == CostClass::Ai {
        next.status = JobStatus::Parked;
        next.last_error = Some(format!("{cause}; AI jobs are never retried"));
    } else if job.attempts >= max {
        next.status = JobStatus::Parked;
        next.last_error = Some(format!(
            "{cause}; parked after {} of {max} attempts",
            job.attempts
        ));
    } else {
        next.status = JobStatus::Queued;
        next.not_before_ms = Some(now_ms.saturating_add(backoff_ms(job.attempts)));
        next.last_error = Some(format!(
            "{cause}; attempt {} of {max} next",
            job.attempts + 1
        ));
    }
    if next.status == JobStatus::Parked {
        next.finished_ms = Some(now_ms);
    }
    next
}

/// Active jobs whose lease ran out, as they should be stored now (Queued or Parked).
pub fn expire_leases(jobs: &[Job], now_ms: UnixMs) -> Vec<Job> {
    jobs.iter()
        .filter(|j| j.status.is_active() && j.lease_expires_ms.is_some_and(|t| t <= now_ms))
        .map(|j| {
            let device = j.device.as_deref().unwrap_or("unknown device");
            retry_or_park(j, now_ms, &format!("lease expired on {device}"))
        })
        .collect()
}

/// The job after an agent's update, or None when the update must be ignored (job already
/// terminal, or sent by a device that does not hold the lease). Coordinator stops and cancels
/// are made terminal when requested, so their later killed update is ignored; a killed
/// update with `owner_stop` is final, any other killed update is a guard kill and retries.
pub fn apply_update(job: &Job, update: &JobUpdate, now_ms: UnixMs) -> Option<Job> {
    if job.status.is_terminal() || job.device.as_deref() != Some(update.device.as_str()) {
        return None;
    }
    let mut next = job.clone();
    next.exit_code = update.exit_code.or(job.exit_code);
    next.output_path = update
        .output_path
        .clone()
        .or_else(|| job.output_path.clone());
    match update.status {
        JobStatus::Leased => return None,
        JobStatus::Running | JobStatus::Paused => {
            next.status = update.status;
            next.started_ms = job.started_ms.or(Some(update.at_ms));
        }
        // Lease returned unstarted: the lease does not count as an attempt.
        JobStatus::Queued => {
            next = Job {
                status: JobStatus::Queued,
                device: None,
                lease_expires_ms: None,
                ..next
            };
            next.attempts = job.attempts.saturating_sub(1);
        }
        // An owner stop (local stop-all) is final, like a coordinator stop.
        JobStatus::Killed if update.owner_stop => finish(&mut next, update),
        // A guard kill or a failure follows the retry policy.
        JobStatus::Failed | JobStatus::Killed => {
            let cause = update
                .error
                .clone()
                .unwrap_or_else(|| format!("job {}", update.status));
            return Some(retry_or_park(&next, now_ms, &cause));
        }
        _ => finish(&mut next, update),
    }
    Some(next)
}

/// Terminal state reported by the agent: no retry, no park.
fn finish(next: &mut Job, update: &JobUpdate) {
    next.status = update.status;
    next.lease_expires_ms = None;
    next.finished_ms = Some(update.at_ms);
    next.last_error = update.error.clone();
}

#[cfg(test)]
mod tests;
