//! Domain: why a queued job may not be leased to a device now, and the quota arithmetic
//! behind the AI part of that. Pure: no I/O, time is a parameter.

use std::collections::BTreeSet;
use std::fmt;

use crate::job_run::allows_program;
use crate::protocol::RunningJob;
use crate::quota::QuotaView;
use crate::types::{
    CostClass, DeviceInfo, DeviceProfile, GuardDecision, Job, JobStatus, TokenClass, UnixMs,
};

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
    /// The device reported an `agent.command_allowlist` that lacks the job's argv[0].
    CommandNotAllowed(String),
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
            Blocker::CommandNotAllowed(p) => {
                write!(f, "command {p} is not in the device's command_allowlist")
            }
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

/// The first reason `job` may not be leased to `ctx.device` now; None when it may.
pub fn blocker(ctx: &SchedContext<'_>, jobs: &[Job], job: &Job) -> Option<Blocker> {
    blockers(ctx, jobs, job).into_iter().next()
}

/// Every reason `job` may not be leased to `ctx.device` now; empty when it may.
pub fn blockers(ctx: &SchedContext<'_>, jobs: &[Job], job: &Job) -> Vec<Blocker> {
    if job.status != JobStatus::Queued {
        return vec![Blocker::NotQueued(job.status)];
    }
    let mut out = job_blockers(ctx.now_ms, ctx.quota, ctx.paused_accounts, jobs, job);
    out.extend(device_blockers(ctx.device, job));
    out
}

/// Reasons a queued job waits on every device alike: retry backoff and AI account state.
pub fn job_blockers(
    now_ms: UnixMs,
    quota: &[QuotaView],
    paused_accounts: &[String],
    jobs: &[Job],
    job: &Job,
) -> Vec<Blocker> {
    let leased = if job.spec.cost == CostClass::Ai {
        ai_leased_accounts(jobs)
    } else {
        BTreeSet::new()
    };
    job_blockers_in(now_ms, quota, paused_accounts, &leased, job)
}

/// `job_blockers` with the leased AI accounts already known, so a caller that asks about many
/// jobs (`pick`, the capacity view) computes them once.
pub fn job_blockers_in(
    now_ms: UnixMs,
    quota: &[QuotaView],
    paused_accounts: &[String],
    leased: &BTreeSet<&str>,
    job: &Job,
) -> Vec<Blocker> {
    let mut out = Vec::new();
    if let Some(until_ms) = job.not_before_ms.filter(|t| *t > now_ms) {
        out.push(Blocker::BackingOff { until_ms });
    }
    if job.spec.cost != CostClass::Ai {
        return out;
    }
    let Some(account) = job.spec.ai_account.as_deref() else {
        out.push(Blocker::NoAiAccount);
        return out;
    };
    let owned = || account.to_string();
    if paused_accounts.iter().any(|a| a == account) {
        out.push(Blocker::AccountPaused(owned()));
    }
    let headroom_pct = spendable_headroom(quota, account);
    let needs_pct = token_class_pct(job.spec.token_class);
    if headroom_pct < needs_pct {
        out.push(Blocker::QuotaShort {
            account: owned(),
            headroom_pct,
            needs_pct,
        });
    }
    if leased.contains(account) {
        out.push(Blocker::AccountLeased(owned()));
    }
    out
}

/// Reasons a queued job cannot run on this particular device.
pub fn device_blockers(dev: &DeviceInfo, job: &Job) -> Vec<Blocker> {
    let spec = &job.spec;
    let mut out = Vec::new();
    if !spec.devices.is_empty() && !spec.devices.contains(&dev.id) {
        out.push(Blocker::DeviceNotAllowed);
    }
    let missing = spec
        .requires
        .iter()
        .filter(|r| !dev.capabilities.contains(r));
    out.extend(missing.map(|r| Blocker::MissingCapability(r.clone())));
    if !cost_allowed(dev.profile, spec.cost) {
        out.push(Blocker::CostNotAllowed(spec.cost));
    }
    if let (Some(list), Some(program)) = (&dev.command_allowlist, spec.command.first()) {
        if !allows_program(list, program) {
            out.push(Blocker::CommandNotAllowed(program.clone()));
        }
    }
    if let Some(account) = spec.ai_account.as_deref() {
        if !dev.ai_accounts.iter().any(|a| a == account) {
            out.push(Blocker::AccountNotOnDevice(account.to_string()));
        }
    }
    out
}
