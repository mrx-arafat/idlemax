//! Domain: the value report, what idlemax did with the machine over a period and what stopped
//! it. Pure: it turns job rows, guard spans and quota snapshots into numbers and one verdict
//! line, and takes the time as a parameter.

mod jobs;
mod quota;
mod reasons;
mod runs;
mod timeline;
mod verdict;

#[cfg(test)]
mod tests;

pub use quota::{QuotaCycle, QuotaSummary};
pub use reasons::{classify, GuardState, HoldReason};
pub use runs::{runs_from_events, JobEvent, JobEventKind, JobRun};
pub use verdict::duration;

use serde::{Deserialize, Serialize};

use crate::types::{
    ControlScope, CostClass, DeviceId, DeviceInfo, Job, JobId, JobSpec, JobStatus, QuotaSnapshot,
    UnixMs,
};

/// The longest period a report covers, in days: the coordinator keeps guard history that long.
pub const MAX_DAYS: i64 = 30;

/// A device whose newest guard span ended longer than this before the period began is taken
/// to have been retired, not asleep, and is left out of the period.
pub const RETIRED_AFTER_MS: i64 = 7 * 86_400_000;

/// Longest label a caller may name a period with.
pub const MAX_LABEL_CHARS: usize = 32;

/// Whether `label` can name a period: short, letters, digits, spaces and `.`, `_`, `-`.
pub fn label_ok(label: &str) -> bool {
    !label.trim().is_empty()
        && label.chars().count() <= MAX_LABEL_CHARS
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c))
}

/// One stretch of time during which a device's guard verdict stayed the same. `end_ms` is
/// the last heartbeat that carried that verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardSpan {
    pub device: DeviceId,
    pub start_ms: UnixMs,
    pub end_ms: UnixMs,
    pub state: GuardState,
}

/// The columns of a job row the report reads, and the spec fields that decide which devices
/// may take the job (the inputs of the scheduler's own `device_blockers`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobFact {
    pub id: JobId,
    pub status: JobStatus,
    pub device: Option<DeviceId>,
    pub created_ms: UnixMs,
    pub not_before_ms: Option<UnixMs>,
    pub started_ms: Option<UnixMs>,
    pub finished_ms: Option<UnixMs>,
    /// When the coordinator logged how the job ended, on its own clock: `finished_ms` comes
    /// from the agent's. None for a job still open, or whose events are gone.
    pub outcome_at_ms: Option<UnixMs>,
    pub devices: Vec<DeviceId>,
    pub requires: Vec<String>,
    pub cost: CostClass,
    pub ai_account: Option<String>,
    /// `argv[0]`, which a device's command allowlist is checked against.
    pub program: Option<String>,
}

impl JobFact {
    /// The job as the scheduler's checks take it, with only the fields they read set.
    pub(crate) fn as_job(&self) -> Job {
        Job {
            id: self.id,
            status: self.status,
            spec: JobSpec {
                devices: self.devices.clone(),
                requires: self.requires.clone(),
                cost: self.cost,
                ai_account: self.ai_account.clone(),
                command: self.program.iter().cloned().collect(),
                ..JobSpec::default()
            },
            ..Job::default()
        }
    }
}

/// A coordinator pause (`idlemax pause`) that was in force from `from_ms` until `to_ms`, or
/// still is when `to_ms` is None.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseSpan {
    pub scope: ControlScope,
    pub from_ms: UnixMs,
    pub to_ms: Option<UnixMs>,
}

/// Everything a report reads. `spans` and `jobs` may come in any order.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub spans: &'a [GuardSpan],
    pub jobs: &'a [JobFact],
    /// The attempts jobs made, from their lifecycle events (see `runs_from_events`): a job
    /// row keeps only its last attempt.
    pub runs: &'a [JobRun],
    /// The coordinator pauses of the period, whose time the scheduler leases nothing in.
    pub pauses: &'a [PauseSpan],
    /// What each device reported about itself, to tell which devices could take a job. A
    /// device missing here is taken to be able to.
    pub devices: &'a [DeviceInfo],
    pub quota: &'a [QuotaSnapshot],
    /// The oldest time the coordinator still has quota snapshots for.
    pub quota_kept_since_ms: UnixMs,
    /// Any AI account is configured on the coordinator.
    pub accounts_configured: bool,
    /// A device silent for longer than this was asleep or offline.
    pub offline_after_ms: i64,
}

/// The period to report on; it ends at `until_ms`, which is also "now".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// How people call it: `today`, `last 7 days`.
    pub label: String,
    pub since_ms: UnixMs,
    pub until_ms: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub label: String,
    pub since_ms: UnixMs,
    pub until_ms: UnixMs,
    /// The answer in one line, the same text in every view.
    pub verdict: String,
    pub jobs: JobSummary,
    pub guard: GuardSummary,
    pub idle: IdleSummary,
    pub quota: QuotaSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobSummary {
    /// Jobs that ran at some point in the period, including one still running.
    pub ran: u64,
    /// Jobs that ended in the period, by how they ended.
    pub succeeded: u64,
    pub failed: u64,
    pub killed: u64,
    pub cancelled: u64,
    /// Wall-clock time the jobs ran within the period, from start to finish.
    pub job_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardSummary {
    /// Device time the guard history covers in the period, asleep or offline included.
    pub tracked_ms: i64,
    /// When the guard history starts, None when there is none yet.
    pub tracking_since_ms: Option<UnixMs>,
    /// Device time the guard held new work.
    pub held_ms: i64,
    /// Held time per reason, longest first. One verdict can have several reasons, so these
    /// can add up to more than `held_ms`.
    pub held_by: Vec<HeldBy>,
    /// Device time with no heartbeat: asleep, or the agent or coordinator was down.
    pub offline_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldBy {
    pub reason: HoldReason,
    pub label: String,
    pub ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IdleSummary {
    /// Device time with at least one job running.
    pub used_ms: i64,
    /// Device time with nothing running, room to run, and nothing waiting.
    pub queue_empty_ms: i64,
    /// Time, counted once however many devices could take the work, with a device idle and
    /// a job it could run waiting past the start grace and not starting.
    pub work_waiting_ms: i64,
    /// The same for AI jobs, which wait for quota or their account, not for room.
    pub quota_waiting_ms: i64,
    /// Queued jobs that no device could take as it is configured, lowest id first (at most 5).
    pub stuck_jobs: Vec<JobId>,
    /// The longest unbroken stretch with nothing running while the device was awake.
    pub longest: Option<Stretch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stretch {
    pub start_ms: UnixMs,
    pub end_ms: UnixMs,
    /// What the stretch mostly was.
    pub cause: IdleCause,
    pub label: String,
}

/// Why nothing ran during an idle stretch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "reason", rename_all = "snake_case")]
pub enum IdleCause {
    /// The guard held new work.
    Held(HoldReason),
    /// A job was waiting, the guard allowed it, and it still did not start.
    WorkWaiting,
    /// An AI job was waiting for quota or its account.
    WaitingOnQuota,
    /// Nothing was queued.
    QueueEmpty,
}

impl IdleCause {
    pub fn label(self) -> String {
        match self {
            IdleCause::Held(r) => format!("held by {}", r.label()),
            IdleCause::WorkWaiting => "work was waiting but did not start".into(),
            IdleCause::WaitingOnQuota => "AI work was waiting for quota".into(),
            IdleCause::QueueEmpty => "the queue was empty".into(),
        }
    }
}

/// The report for `window`.
pub fn build(window: &Window, input: &Inputs<'_>) -> Report {
    let times = jobs::times(input.jobs, input.runs, input.pauses, window.until_ms);
    let (guard, idle) = timeline::analyze(window, input, &times);
    let mut report = Report {
        label: window.label.clone(),
        since_ms: window.since_ms,
        until_ms: window.until_ms,
        verdict: String::new(),
        jobs: jobs::summarize(&times, window),
        guard,
        idle,
        quota: quota::summarize(
            input.quota,
            input.accounts_configured,
            window,
            input.quota_kept_since_ms,
        ),
    };
    report.verdict = verdict::line(&report);
    report
}
