//! Domain: job requests and their lifecycle (cost classes, spec and its validation, status, job).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{DeviceId, JobId, UnixMs};
use crate::worker::WorkerSpec;

/// What a job does when the user becomes active on its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivePolicy {
    #[default]
    Pause,
    Continue,
    Kill,
}

/// How a job is admitted to start on a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    /// The guard decides: idle user, memory pressure, slots and RAM budget.
    #[default]
    Guard,
    /// The dispatcher's own jobs: the worker lines of D14 decide (A16). Only the
    /// coordinator sets it.
    Lines,
}

impl Admission {
    pub fn is_guard(&self) -> bool {
        *self == Admission::Guard
    }
}

/// True for a job the dispatcher created: its orchestrator pass, setup and check jobs.
pub fn is_dispatcher_job(spec: &JobSpec) -> bool {
    spec.admission == Admission::Lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostClass {
    #[default]
    CpuLight,
    CpuHeavy,
    Io,
    Ai,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenClass {
    Small,
    Medium,
    Large,
}

pub const DEFAULT_MEM_MB: u64 = 512;
pub const DEFAULT_MAX_RUNTIME_SECS: u64 = 1800;
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// Longest idempotency key, in characters.
pub const MAX_KEY_CHARS: usize = 128;

/// A job request. `command` is argv, never run through a shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobSpec {
    pub name: String,
    /// Idempotency key: while a job with this key has not finished, adding the same key
    /// again returns that job instead of queueing a duplicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub command: Vec<String>,
    /// Optional standard input, kept out of argv and operator job responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    /// Capabilities the device must have.
    pub requires: Vec<String>,
    /// Allowed device ids; empty means any device.
    pub devices: Vec<DeviceId>,
    /// Higher runs first.
    pub priority: i32,
    pub cost: CostClass,
    pub mem_mb: u64,
    pub max_runtime_secs: u64,
    pub ai_account: Option<String>,
    pub token_class: Option<TokenClass>,
    pub on_user_active: ActivePolicy,
    pub max_attempts: u32,
    /// Absent on the wire for a plain job, so an agent that predates it treats every job as
    /// a plain one, which is the safe side.
    #[serde(skip_serializing_if = "Admission::is_guard")]
    pub admission: Admission,
    /// Set only by the dispatcher: this job is one launch, resume, keep or discard of a Run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<WorkerSpec>,
}

impl Default for JobSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            key: None,
            command: Vec::new(),
            stdin: None,
            cwd: None,
            env: BTreeMap::new(),
            requires: Vec::new(),
            devices: Vec::new(),
            priority: 0,
            cost: CostClass::default(),
            mem_mb: DEFAULT_MEM_MB,
            max_runtime_secs: DEFAULT_MAX_RUNTIME_SECS,
            ai_account: None,
            token_class: None,
            on_user_active: ActivePolicy::default(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            admission: Admission::Guard,
            worker: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpecError {
    #[error("name is empty")]
    EmptyName,
    #[error("command is empty")]
    EmptyCommand,
    #[error("{0} must be > 0")]
    Zero(&'static str),
    #[error("cost ai needs ai_account")]
    AiWithoutAccount,
    #[error("ai_account is only allowed with cost ai")]
    AccountWithoutAi,
    #[error("a worker section needs cost ai")]
    WorkerWithoutAi,
    #[error("key is empty")]
    EmptyKey,
    #[error("key is longer than 128 characters")]
    KeyTooLong,
    #[error("{field} is over the limit of {limit} {unit}")]
    OverLimit {
        field: &'static str,
        limit: usize,
        unit: &'static str,
    },
    #[error("{0} contains a control character")]
    ControlChar(&'static str),
    #[error("{0} contains a NUL byte")]
    Nul(&'static str),
    #[error("env name must be non-empty and have no '=' or NUL byte")]
    BadEnvName,
}

impl JobSpec {
    /// Rejects specs the coordinator must never queue.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.name.trim().is_empty() {
            return Err(SpecError::EmptyName);
        }
        if self.command.first().is_none_or(|c| c.trim().is_empty()) {
            return Err(SpecError::EmptyCommand);
        }
        if let Some(key) = &self.key {
            if key.trim().is_empty() {
                return Err(SpecError::EmptyKey);
            }
            if key.chars().count() > MAX_KEY_CHARS {
                return Err(SpecError::KeyTooLong);
            }
        }
        for (field, value) in [
            ("mem_mb", self.mem_mb),
            ("max_runtime_secs", self.max_runtime_secs),
            ("max_attempts", u64::from(self.max_attempts)),
        ] {
            if value == 0 {
                return Err(SpecError::Zero(field));
            }
        }
        match (self.cost, &self.ai_account) {
            (CostClass::Ai, None) => return Err(SpecError::AiWithoutAccount),
            (c, Some(_)) if c != CostClass::Ai => return Err(SpecError::AccountWithoutAi),
            _ => {}
        }
        if self.worker.is_some() && self.cost != CostClass::Ai {
            return Err(SpecError::WorkerWithoutAi);
        }
        crate::spec_limits::check(self)
    }

    /// AI jobs never retry automatically.
    pub fn effective_max_attempts(&self) -> u32 {
        if self.cost == CostClass::Ai {
            1
        } else {
            self.max_attempts
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    #[default]
    Queued,
    Leased,
    Running,
    Paused,
    Succeeded,
    Failed,
    Killed,
    Cancelled,
    /// Gave up after max_attempts; needs a human.
    Parked,
}

impl JobStatus {
    pub const ALL: [JobStatus; 9] = [
        JobStatus::Queued,
        JobStatus::Leased,
        JobStatus::Running,
        JobStatus::Paused,
        JobStatus::Succeeded,
        JobStatus::Failed,
        JobStatus::Killed,
        JobStatus::Cancelled,
        JobStatus::Parked,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Succeeded
                | JobStatus::Failed
                | JobStatus::Killed
                | JobStatus::Cancelled
                | JobStatus::Parked
        )
    }

    /// Holds a lease on a device (Leased, Running, Paused).
    pub fn is_active(self) -> bool {
        matches!(
            self,
            JobStatus::Leased | JobStatus::Running | JobStatus::Paused
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Leased => "leased",
            JobStatus::Running => "running",
            JobStatus::Paused => "paused",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Killed => "killed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::Parked => "parked",
        }
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for JobStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        JobStatus::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| format!("unknown job status: {s}"))
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: JobId,
    pub spec: JobSpec,
    pub status: JobStatus,
    pub device: Option<DeviceId>,
    pub attempts: u32,
    pub created_ms: UnixMs,
    pub started_ms: Option<UnixMs>,
    pub finished_ms: Option<UnixMs>,
    pub lease_expires_ms: Option<UnixMs>,
    /// Retry backoff: a Queued job is not leased before this time.
    pub not_before_ms: Option<UnixMs>,
    pub exit_code: Option<i32>,
    pub last_error: Option<String>,
    /// Inbox markdown path on the device that ran it.
    pub output_path: Option<String>,
}
