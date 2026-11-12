//! Domain: shared contract types (devices, metrics, jobs, guard, control, quota, events, advice).

use serde::{Deserialize, Serialize};

mod device;
mod guard;
mod job;

pub use device::{DeviceInfo, DeviceProfile, MemoryPressure, MetricsSnapshot, Os};
pub use guard::{GuardAction, GuardDecision, MemoryLimit};
pub use job::{
    is_dispatcher_job, ActivePolicy, Admission, CostClass, Job, JobSpec, JobStatus, SpecError,
    TokenClass, DEFAULT_MAX_ATTEMPTS, DEFAULT_MAX_RUNTIME_SECS, DEFAULT_MEM_MB, MAX_KEY_CHARS,
};

pub type DeviceId = String;
pub type JobId = i64;
/// Unix time in milliseconds. Domain code takes it as a parameter; adapters read the
/// clock with `crate::state::now_ms` in the bin crate.
pub type UnixMs = i64;

/// Target of a control action. JSON: {"type":"device","id":"mac"}, {"type":"global"}.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum ControlScope {
    Global,
    Device(DeviceId),
    Account(String),
    Job(JobId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Pause,
    Resume,
    StopAll,
    Cancel,
}

/// JSON: {"kind":"rolling","minutes":300} or {"kind":"weekly"}.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuotaWindowKind {
    Rolling { minutes: u32 },
    Weekly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeSource {
    /// Claude Code statusline `rate_limits` capture file.
    ClaudeStatusline,
    /// Newest `~/.codex/sessions/**/*.jsonl` rate_limits event.
    CodexSessions,
    #[default]
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub account: String,
    pub window: QuotaWindowKind,
    pub used_pct: f64,
    #[serde(default)]
    pub resets_at_ms: Option<UnixMs>,
    #[serde(default)]
    pub source: ProbeSource,
    pub at_ms: UnixMs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    DeviceOnline,
    DeviceOffline,
    JobQueued,
    JobLeased,
    JobStarted,
    JobPaused,
    JobResumed,
    JobSucceeded,
    JobFailed,
    JobKilled,
    JobCancelled,
    JobParked,
    LeaseExpired,
    Guard,
    Control,
    Quota,
    Advice,
    /// A schedule queued its next job.
    ScheduleFired,
    Error,
    #[default]
    Info,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub id: i64,
    pub at_ms: UnixMs,
    pub kind: EventKind,
    pub device: Option<DeviceId>,
    pub job: Option<JobId>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationKind {
    Assign,
    #[default]
    Do,
    Fix,
    FollowUp,
    Warn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationSource {
    #[default]
    Rules,
    Ai,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationStatus {
    #[default]
    Open,
    Accepted,
    Dismissed,
}

/// Advisor output. Accepting it queues `suggested_job`; nothing runs on its own.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recommendation {
    pub id: i64,
    pub at_ms: UnixMs,
    /// Stable dedup key, e.g. "parked_job:42" or "pr_waiting:org/repo#12".
    pub key: String,
    pub kind: RecommendationKind,
    pub title: String,
    pub why: String,
    pub evidence: Vec<String>,
    pub suggested_job: Option<JobSpec>,
    /// 0.0 to 1.0.
    pub confidence: f64,
    pub source: RecommendationSource,
    pub status: RecommendationStatus,
}

#[cfg(test)]
mod tests;
