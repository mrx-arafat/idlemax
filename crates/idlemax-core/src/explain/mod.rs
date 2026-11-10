//! Domain: why a job is not running right now, as plain-language reasons with the fix.
//! It asks the scheduler's own checks (`job_blockers`, `device_blockers`, `start_budget`,
//! `pick`) instead of repeating them, so the explanation cannot disagree with what the
//! scheduler does. Pure: no I/O, time is a parameter.

mod guard_text;
mod reasons;
mod summary;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::control::{may_assign, paused_accounts};
use crate::protocol::DeviceView;
use crate::quota::QuotaView;
use crate::scheduler::{self, SchedContext};
use crate::types::{
    ControlScope, DeviceId, DeviceInfo, GuardAction, GuardDecision, Job, JobId, JobSpec, JobStatus,
    UnixMs,
};

pub(crate) use self::reasons::ineligible_causes;
use self::reasons::{blocker_reason, hold_reasons, span};
pub use self::summary::{summarize, Headline, WhySummary};

/// The coarse answer: can it start, is it stuck, is it running, is it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainState {
    /// Nothing blocks it; a device leases it on its next heartbeat.
    Ready,
    /// Queued or parked, with at least one reason it will not start.
    Blocked,
    /// Leased, running or paused on a device.
    Active,
    /// Succeeded, failed, killed or cancelled.
    Finished,
}

/// Stable machine-readable family of a reason, for scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    GlobalPause,
    AccountPause,
    JobPause,
    DevicePause,
    LocalPause,
    Backoff,
    Parked,
    NoDeviceOnline,
    Offline,
    DeviceFilter,
    MissingCapability,
    CostClass,
    CommandNotAllowed,
    AiNoAccount,
    AiAccountNotOnDevice,
    AiQuota,
    AiAccountBusy,
    Guard,
    NoSlot,
    RamBudget,
    /// Eligible, but jobs ahead in the queue take the free slot or RAM first.
    QueueOrder,
}

impl ReasonKind {
    /// Holds until a job spec or a device's config changes, not by waiting.
    pub fn is_permanent(self) -> bool {
        matches!(
            self,
            ReasonKind::Parked
                | ReasonKind::DeviceFilter
                | ReasonKind::MissingCapability
                | ReasonKind::CostClass
                | ReasonKind::CommandNotAllowed
                | ReasonKind::AiNoAccount
                | ReasonKind::AiAccountNotOnDevice
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reason {
    pub kind: ReasonKind,
    pub message: String,
    /// What to do about it; None when it clears on its own.
    pub fix: Option<String>,
    /// When the reason ends by itself (retry backoff).
    pub until_ms: Option<UnixMs>,
}

/// Reasons that apply to one device only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceExplanation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<crate::types::MemoryLimit>,
    /// Actual idle-sleep assertion state as of the last heartbeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_awake: Option<String>,
    pub device: DeviceId,
    pub online: bool,
    /// The verdicts below are as of this heartbeat (guard, running jobs, budget).
    pub last_seen_ms: UnixMs,
    /// This device would lease the job on its next heartbeat.
    pub ready: bool,
    /// Why this device can never take the job until the job spec or the device's config
    /// changes, one short phrase per cause ("limited to mac", "no cargo"). Empty when it is
    /// eligible: whatever else holds the job there clears by itself.
    #[serde(default)]
    pub ineligible: Vec<String>,
    pub reasons: Vec<Reason>,
}

/// GET /v1/jobs/{id}/why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Explanation {
    pub job: JobId,
    pub name: String,
    pub status: JobStatus,
    pub state: ExplainState,
    pub summary: String,
    /// Reasons that hold on every device.
    pub reasons: Vec<Reason>,
    pub devices: Vec<DeviceExplanation>,
    pub at_ms: UnixMs,
}

/// The coordinator's view at one instant.
#[derive(Debug, Clone, Copy)]
pub struct ExplainInput<'a> {
    pub now_ms: UnixMs,
    pub job: &'a Job,
    /// The scheduler's own input: every active job and every queued job that no pause
    /// holds (`pick` never sees held jobs). The caller builds it once and shares it.
    pub jobs: &'a [Job],
    pub devices: &'a [DeviceView],
    pub paused: &'a [ControlScope],
    pub quota: &'a [QuotaView],
}

pub fn explain(input: &ExplainInput<'_>) -> Explanation {
    let job = input.job;
    let mut out = Explanation {
        job: job.id,
        name: job.spec.name.clone(),
        status: job.status,
        state: ExplainState::Finished,
        summary: String::new(),
        reasons: Vec::new(),
        devices: Vec::new(),
        at_ms: input.now_ms,
    };
    match job.status {
        JobStatus::Queued => explain_queued(input, &mut out),
        JobStatus::Parked => explain_parked(job, &mut out),
        s if s.is_active() => explain_active(input, &mut out),
        _ => out.summary = finished_summary(job),
    }
    out
}

fn explain_queued(input: &ExplainInput<'_>, out: &mut Explanation) {
    let job = input.job;
    let visible = input.jobs;
    let mut reasons = hold_reasons(input);
    let accounts = paused_accounts(input.paused);
    let blockers = scheduler::job_blockers(input.now_ms, input.quota, &accounts, visible, job);
    reasons.extend(
        blockers
            .iter()
            .filter_map(|b| blocker_reason(b, input, None)),
    );
    reasons.extend(reasons::cluster_reasons(input));
    let job_wide = !reasons.is_empty();
    let devices: Vec<DeviceExplanation> = input
        .devices
        .iter()
        .map(|dv| explain_device(input, visible, dv, &accounts, job_wide))
        .collect();
    let ready: Vec<String> = input
        .devices
        .iter()
        .zip(&devices)
        .filter(|(_, d)| d.ready)
        .map(|(dv, d)| {
            let age = span(input.now_ms - dv.last_seen_ms);
            format!("{} (last heartbeat {age} ago)", d.device)
        })
        .collect();
    out.state = if ready.is_empty() {
        ExplainState::Blocked
    } else {
        ExplainState::Ready
    };
    let head = headline(job);
    out.summary = if ready.is_empty() {
        format!("{head} is queued but not starting.")
    } else {
        format!(
            "{head} is queued and nothing blocks it: {} should lease it on its next heartbeat.",
            ready.join(" or ")
        )
    };
    out.reasons = reasons;
    out.devices = devices;
}

fn explain_device(
    input: &ExplainInput<'_>,
    visible: &[Job],
    dv: &DeviceView,
    accounts: &[String],
    job_wide: bool,
) -> DeviceExplanation {
    let job = input.job;
    let fixed = scheduler::device_blockers(&dv.info, job);
    let ineligible = reasons::ineligible_causes(&fixed, job);
    let mut reasons: Vec<Reason> = fixed
        .iter()
        .filter_map(|b| blocker_reason(b, input, Some(&dv.info)))
        .collect();
    if dv.online {
        let guard = guard_now(dv, visible);
        let ctx = SchedContext {
            now_ms: input.now_ms,
            device: &dv.info,
            guard: &guard,
            running: &dv.running,
            quota: input.quota,
            paused: !may_assign(input.paused, &dv.info.id, dv.local_paused),
            paused_accounts: accounts,
        };
        reasons.extend(online_reasons(input, visible, dv, &ctx));
        if reasons.is_empty() && !job_wide {
            reasons.extend(reasons::queue_order(job, &scheduler::pick(&ctx, visible)));
        }
    } else {
        reasons.insert(0, reasons::offline(dv, input.now_ms));
    }
    DeviceExplanation {
        memory_limit: dv.guard.as_ref().and_then(|g| g.memory_limit.clone()),
        keep_awake: dv.keep_awake.clone(),
        device: dv.info.id.clone(),
        online: dv.online,
        last_seen_ms: dv.last_seen_ms,
        ready: dv.online && reasons.is_empty() && !job_wide,
        ineligible,
        reasons,
    }
}

/// The device's last guard verdict, minus the jobs the coordinator knows it started after
/// that report. The scheduler needs no such correction: it always sees a fresh heartbeat.
pub(crate) fn guard_now(dv: &DeviceView, visible: &[Job]) -> GuardDecision {
    let mut guard = dv.guard.clone().unwrap_or_default();
    let (started, mem_mb) = visible
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Running | JobStatus::Paused))
        .filter(|j| j.device.as_deref() == Some(dv.info.id.as_str()))
        .filter(|j| !dv.running.iter().any(|r| r.id == j.id))
        .fold((0u32, 0u64), |(n, mem), j| (n + 1, mem + j.spec.mem_mb));
    if let GuardAction::Run { slots, mem_mib } = guard.action {
        guard.action = GuardAction::Run {
            slots: slots.saturating_sub(started),
            mem_mib: mem_mib.saturating_sub(mem_mb),
        };
    }
    guard
}

/// Pauses, guard verdict and budget of a device that is heartbeating.
fn online_reasons(
    input: &ExplainInput<'_>,
    visible: &[Job],
    dv: &DeviceView,
    ctx: &SchedContext<'_>,
) -> Vec<Reason> {
    let mut out = Vec::new();
    if input
        .paused
        .contains(&ControlScope::Device(dv.info.id.clone()))
    {
        out.push(reasons::device_pause(&dv.info.id));
    }
    out.extend(reasons::guard_reasons(dv, ctx.guard));
    if let Some((slots, mem_left)) = scheduler::start_budget(ctx, visible) {
        if slots == 0 {
            out.push(reasons::no_slot(&dv.info.id, visible));
        }
        if input.job.spec.mem_mb > mem_left {
            out.push(reasons::ram_budget(input.job.spec.mem_mb, mem_left));
        }
    }
    out
}

fn explain_parked(job: &Job, out: &mut Explanation) {
    out.state = ExplainState::Blocked;
    out.summary = format!(
        "{} is parked and will not run again by itself.",
        headline(job)
    );
    out.reasons.push(reasons::parked(job));
}

fn explain_active(input: &ExplainInput<'_>, out: &mut Explanation) {
    let job = input.job;
    let on = job.device.as_deref().unwrap_or("an unknown device");
    out.state = ExplainState::Active;
    out.summary = match job.status {
        JobStatus::Running => format!(
            "{} is running on {on} for {}.",
            headline(job),
            span(input.now_ms - job.started_ms.unwrap_or(input.now_ms))
        ),
        JobStatus::Paused => format!("{} is paused on {on}.", headline(job)),
        _ => format!(
            "{} is leased to {on}, waiting for its agent to start it.",
            headline(job)
        ),
    };
    out.reasons = hold_reasons(input);
    let dv = input
        .devices
        .iter()
        .find(|d| Some(&d.info.id) == job.device.as_ref());
    if let Some(dv) = dv.filter(|d| !d.online) {
        out.reasons.push(reasons::offline(dv, input.now_ms));
    } else if let (Some(dv), JobStatus::Paused) = (dv, job.status) {
        let guard = dv.guard.clone().unwrap_or_default();
        out.reasons.extend(reasons::guard_reasons(dv, &guard));
    }
}

/// "Job 7 (nightly-fmt)".
fn headline(job: &Job) -> String {
    format!("Job {} ({})", job.id, job.spec.name)
}

fn finished_summary(job: &Job) -> String {
    let mut s = format!("{} {}", headline(job), job.status);
    if let Some(code) = job.exit_code {
        s.push_str(&format!(" (exit {code})"));
    }
    // "exit code N" would only repeat the code just shown.
    let error = job
        .last_error
        .as_deref()
        .filter(|e| job.exit_code.is_none_or(|c| *e != format!("exit code {c}")));
    match error {
        Some(e) => s.push_str(&format!(": {e}.")),
        None => s.push('.'),
    }
    if matches!(job.status, JobStatus::Failed | JobStatus::Killed) {
        s.push_str(" It will not run again by itself.");
    }
    s
}

/// A warning for `job add` when every device that could run the job reported an
/// `agent.command_allowlist` without its command, so it would wait in the queue forever.
/// None when any candidate allows it, or may allow it (an agent that predates the report).
pub fn allowlist_warning(spec: &JobSpec, devices: &[DeviceInfo]) -> Option<String> {
    let program = spec.command.first()?;
    let candidates: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|d| spec.devices.is_empty() || spec.devices.contains(&d.id))
        .collect();
    let all_refuse = !candidates.is_empty()
        && candidates.iter().all(|d| {
            d.command_allowlist
                .as_ref()
                .is_some_and(|list| !crate::job_run::allows_program(list, program))
        });
    all_refuse.then(|| {
        let names: Vec<&str> = candidates.iter().map(|d| d.id.as_str()).collect();
        format!(
            "no device allows \"{program}\" (checked: {}). On the device that should run it, run \
             `idlemax config set agent.command_allowlist <programs>` (list every program it may \
             run), or the job will wait in the queue.",
            names.join(", ")
        )
    })
}
