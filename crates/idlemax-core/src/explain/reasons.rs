//! Domain: the wording of each reason and the command or setting that clears it.

use super::guard_text;
use super::{ExplainInput, Reason, ReasonKind};
use crate::control::in_scope;
use crate::protocol::DeviceView;
use crate::scheduler::Blocker;
use crate::types::{
    ControlScope, CostClass, DeviceInfo, DeviceProfile, GuardAction, GuardDecision, Job, JobId,
    UnixMs,
};

impl Reason {
    pub(super) fn new(kind: ReasonKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            fix: None,
            until_ms: None,
        }
    }

    pub(super) fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    fn until(mut self, at_ms: UnixMs) -> Self {
        self.until_ms = Some(at_ms);
        self
    }
}

/// "3h 5m", "2m 10s", "40s".
pub(super) fn span(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match (s / 3600, s % 3600 / 60, s % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

/// Control pauses (`idlemax pause ...`) that cover the job.
pub(super) fn hold_reasons(input: &ExplainInput<'_>) -> Vec<Reason> {
    let job = input.job;
    input
        .paused
        .iter()
        .filter(|scope| in_scope(scope, job))
        .map(|scope| match scope {
            ControlScope::Global => {
                Reason::new(ReasonKind::GlobalPause, "all jobs are paused").fix("idlemax resume")
            }
            ControlScope::Device(d) => device_pause(d),
            ControlScope::Account(a) => Reason::new(
                ReasonKind::AccountPause,
                format!("AI account {a} is paused"),
            )
            .fix(format!("idlemax resume --account {a}")),
            ControlScope::Job(id) => {
                Reason::new(ReasonKind::JobPause, format!("job {id} is paused"))
                    .fix(format!("idlemax resume --job {id}"))
            }
        })
        .collect()
}

pub(super) fn device_pause(id: &str) -> Reason {
    Reason::new(
        ReasonKind::DevicePause,
        format!("{id} is paused by the owner"),
    )
    .fix(format!("idlemax resume --device {id}"))
}

/// No online device at all, or a `devices` list that names no known device.
pub(super) fn cluster_reasons(input: &ExplainInput<'_>) -> Vec<Reason> {
    let mut out = Vec::new();
    if input.devices.iter().all(|d| !d.online) {
        let (message, fix) = if input.devices.is_empty() {
            (
                "no device has connected to the coordinator yet".to_string(),
                "start `idlemax agent` on a device (see `idlemax init`)",
            )
        } else {
            (
                "every device is offline".to_string(),
                "check the agents are running and can reach the coordinator",
            )
        };
        out.push(Reason::new(ReasonKind::NoDeviceOnline, message).fix(fix));
    }
    let wanted = &input.job.spec.devices;
    if !wanted.is_empty()
        && !wanted
            .iter()
            .any(|w| input.devices.iter().any(|d| &d.info.id == w))
    {
        let known: Vec<&str> = input.devices.iter().map(|d| d.info.id.as_str()).collect();
        out.push(
            Reason::new(
                ReasonKind::DeviceFilter,
                format!(
                    "the job is limited to {} but no device with that id has connected",
                    wanted.join(", ")
                ),
            )
            .fix(format!(
                "fix devices in the job spec (known devices: {})",
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            )),
        );
    }
    out
}

/// A scheduler blocker in words. `device` is the device being checked, None for job-wide ones.
/// Returns None when another reason already covers it.
pub(super) fn blocker_reason(
    b: &Blocker,
    input: &ExplainInput<'_>,
    device: Option<&DeviceInfo>,
) -> Option<Reason> {
    let job = input.job;
    let on = device.map_or("that device", |d| d.id.as_str());
    Some(match b {
        Blocker::BackingOff { until_ms } => backoff(job, *until_ms, input.now_ms),
        // Only for jobs that are not queued, which `explain` answers before asking blockers.
        Blocker::NotQueued(_) => return None,
        // Reported once, with its resume command, by `hold_reasons`.
        Blocker::AccountPaused(_) => return None,
        Blocker::DeviceNotAllowed => Reason::new(
            ReasonKind::DeviceFilter,
            format!("the job is limited to {}", job.spec.devices.join(", ")),
        )
        .fix(format!(
            "add {on} to devices in the job spec, or leave devices empty"
        )),
        Blocker::MissingCapability(c) => Reason::new(
            ReasonKind::MissingCapability,
            format!("{on} does not have \"{c}\" on its PATH, and the job requires it"),
        )
        .fix(format!(
            "install {c} on {on} and restart its agent, or remove it from requires"
        )),
        Blocker::CostNotAllowed(c) => cost_class(*c, device, on),
        Blocker::CommandNotAllowed(p) => Reason::new(
            ReasonKind::CommandNotAllowed,
            format!("\"{p}\" is not in {on}'s agent.command_allowlist, so it would refuse the job"),
        )
        .fix(format!(
            "on {on}, run `idlemax config set agent.command_allowlist <programs>` and list \"{p}\" with every program it may already run"
        )),
        Blocker::NoAiAccount => {
            Reason::new(ReasonKind::AiNoAccount, "this AI job names no ai_account")
                .fix("set ai_account in the job spec")
        }
        Blocker::AccountNotOnDevice(a) => Reason::new(
            ReasonKind::AiAccountNotOnDevice,
            format!("{on} may not spend AI account {a}"),
        )
        .fix(format!(
            "add \"{a}\" to agent.ai_accounts in config.toml on {on} and restart its agent"
        )),
        Blocker::QuotaShort {
            account,
            headroom_pct,
            needs_pct,
        } => quota_short(account, *headroom_pct, *needs_pct, input),
        Blocker::AccountLeased(a) => account_busy(a, input),
    })
}

/// One short phrase per device-level blocker: what makes a device unable to ever take the
/// job. These are the scheduler's static checks, so none of them clears by waiting.
pub(super) fn ineligible_causes(blockers: &[Blocker], job: &Job) -> Vec<String> {
    blockers
        .iter()
        .filter_map(|b| match b {
            Blocker::DeviceNotAllowed => {
                Some(format!("limited to {}", job.spec.devices.join(", ")))
            }
            Blocker::MissingCapability(c) => Some(format!("no {c}")),
            Blocker::CostNotAllowed(c) => Some(format!("does not take {} jobs", cost_name(*c))),
            Blocker::CommandNotAllowed(p) => Some(format!("{p} is not on its allowlist")),
            Blocker::AccountNotOnDevice(a) => Some(format!("may not spend {a}")),
            _ => None,
        })
        .collect()
}

fn cost_name(cost: CostClass) -> &'static str {
    match cost {
        CostClass::CpuLight => "cpu_light",
        CostClass::CpuHeavy => "cpu_heavy",
        CostClass::Io => "io",
        CostClass::Ai => "ai",
    }
}

fn cost_class(cost: CostClass, device: Option<&DeviceInfo>, on: &str) -> Reason {
    let profile = device.map_or(DeviceProfile::LinuxServer, |d| d.profile);
    let kind = format!("{cost:?}");
    Reason::new(
        ReasonKind::CostClass,
        format!("{on} has the {profile:?} profile, which does not take {kind} jobs (servers take only CpuLight and Io)"),
    )
    .fix("set cost to cpu_light or io in the job spec, or run it on a laptop device")
}

fn backoff(job: &Job, until_ms: UnixMs, now_ms: UnixMs) -> Reason {
    let max = job.spec.effective_max_attempts();
    let mut msg = format!(
        "waiting out the retry delay: attempt {} of {max} starts in {}",
        job.attempts + 1,
        span(until_ms - now_ms)
    );
    if let Some(cause) = job.last_error.as_deref().map(failure_cause) {
        msg.push_str(&format!(" (last error: {cause})"));
    }
    Reason::new(ReasonKind::Backoff, msg).until(until_ms)
}

/// `retry_or_park` stores "<cause>; attempt N of M next"; the attempt part is said already.
fn failure_cause(last_error: &str) -> &str {
    last_error
        .rsplit_once("; attempt ")
        .map_or(last_error, |(cause, _)| cause)
}

fn quota_short(account: &str, headroom: f64, needs: f64, input: &ExplainInput<'_>) -> Reason {
    let mut why: Vec<&str> = Vec::new();
    for view in input
        .quota
        .iter()
        .filter(|v| v.account == account && !v.spendable)
    {
        for r in &view.reasons {
            if !why.contains(&r.as_str()) {
                why.push(r);
            }
        }
    }
    let mut msg = format!(
        "AI account {account} has {headroom:.0}% spendable quota, the job needs about {needs:.0}%"
    );
    if !why.is_empty() {
        msg.push_str(&format!(": {}", why.join("; ")));
    }
    Reason::new(ReasonKind::AiQuota, msg).fix(
        "nothing to do: idlemax spends only quota left over before a window resets (see `idlemax quota`); a smaller token_class needs less headroom",
    )
}

fn account_busy(account: &str, input: &ExplainInput<'_>) -> Reason {
    let holder = input.jobs.iter().find(|j| {
        j.status.is_active()
            && j.spec.cost == CostClass::Ai
            && j.spec.ai_account.as_deref() == Some(account)
    });
    let who = holder.map_or(String::new(), |j| format!(" (job #{})", j.id));
    Reason::new(
        ReasonKind::AiAccountBusy,
        format!("another AI job holds AI account {account}{who}; only one runs per account"),
    )
    .fix("it starts after that job finishes (`idlemax jobs --status running`)")
}

pub(super) fn offline(dv: &DeviceView, now_ms: UnixMs) -> Reason {
    Reason::new(
        ReasonKind::Offline,
        format!(
            "{} is offline: last heartbeat {} ago",
            dv.info.id,
            span(now_ms - dv.last_seen_ms)
        ),
    )
    .fix(format!(
        "start `idlemax agent` on {} (is it awake and on the network?)",
        dv.info.id
    ))
}

/// The guard's reasons while it holds, pauses or kills; a local pause the guard did not name.
pub(super) fn guard_reasons(dv: &DeviceView, guard: &GuardDecision) -> Vec<Reason> {
    let mut out = Vec::new();
    if !matches!(guard.action, GuardAction::Run { .. }) {
        out.extend(guard.reasons.iter().map(|r| guard_text::reason(r)));
        if out.is_empty() {
            out.push(Reason::new(
                ReasonKind::Guard,
                format!("the guard on {} is not letting new jobs start", dv.info.id),
            ));
        }
    }
    if dv.local_paused && !out.iter().any(|r| r.kind == ReasonKind::LocalPause) {
        out.push(guard_text::local_pause());
    }
    out
}

pub(super) fn no_slot(id: &str, visible: &[Job]) -> Reason {
    let holders: Vec<String> = visible
        .iter()
        .filter(|j| j.status.is_active() && j.device.as_deref() == Some(id))
        .map(|j| format!("#{}", j.id))
        .collect();
    let message = match holders.as_slice() {
        [] => format!("{id} has no free job slot"),
        [one] => format!("{id} has no free job slot: held by job {one}"),
        many => format!(
            "{id} has no free job slot: held by jobs {}",
            many.join(", ")
        ),
    };
    Reason::new(ReasonKind::NoSlot, message)
        .fix("it starts when one finishes; raise guard.max_jobs in config.toml on that device for more at once")
}

pub(super) fn ram_budget(need_mib: u64, left_mib: u64) -> Reason {
    Reason::new(
        ReasonKind::RamBudget,
        format!("the job needs {need_mib} MiB but only {left_mib} MiB of the device's RAM budget is free"),
    )
    .fix("lower mem_mb in the job spec, or raise guard.ram_budget_mib in config.toml on that device")
}

/// Eligible on this device, yet the scheduler would start `picked` first.
pub(super) fn queue_order(job: &Job, picked: &[JobId]) -> Option<Reason> {
    if picked.contains(&job.id) {
        return None;
    }
    let ahead: Vec<String> = picked.iter().map(|id| format!("#{id}")).collect();
    Some(
        Reason::new(
            ReasonKind::QueueOrder,
            format!(
                "jobs {} go first and use the free slot or RAM budget",
                ahead.join(", ")
            ),
        )
        .fix("it starts when they finish; raise priority in the job spec to go first"),
    )
}

pub(super) fn parked(job: &Job) -> Reason {
    let max = job.spec.effective_max_attempts();
    let message = match &job.last_error {
        Some(e) => format!("last error: {e}"),
        None => format!("parked after {} of {max} attempts", job.attempts),
    };
    Reason::new(ReasonKind::Parked, message)
        .fix("fix the cause, then queue it again with `idlemax job add <spec.toml>`")
}
