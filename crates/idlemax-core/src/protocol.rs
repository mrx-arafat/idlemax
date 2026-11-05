//! Wire types for the coordinator and agent HTTP APIs.

use serde::{Deserialize, Serialize};

use crate::types::{
    ControlAction, ControlScope, CostClass, DeviceId, DeviceInfo, GuardDecision, Job, JobId,
    JobStatus, MetricsSnapshot, QuotaSnapshot, UnixMs,
};

/// A job the agent is currently running or holding paused.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunningJob {
    pub id: JobId,
    pub name: String,
    /// Running or Paused.
    pub status: JobStatus,
    pub pid: Option<u32>,
    pub started_ms: UnixMs,
    /// Budgeted memory from the spec, counted against the guard RAM budget.
    pub mem_mb: u64,
    pub rss_bytes: Option<u64>,
    pub cpu_pct: Option<f64>,
    pub cost: CostClass,
    pub ai_account: Option<String>,
}

/// POST /v1/agent/heartbeat, every `agent.heartbeat_secs`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Heartbeat {
    pub device: DeviceInfo,
    pub metrics: MetricsSnapshot,
    pub guard: GuardDecision,
    pub running: Vec<RunningJob>,
    pub quota: Vec<QuotaSnapshot>,
    /// (cli name, newest session file mtime), e.g. ("claude", 1730000000000).
    pub interactive_use: Vec<(String, UnixMs)>,
    /// Kill switch file present or a local pause is active.
    pub local_paused: bool,
}

/// JSON: {"cmd":"pause_job","id":7}, {"cmd":"stop_all"}.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum AgentCommand {
    PauseJob { id: JobId },
    ResumeJob { id: JobId },
    KillJob { id: JobId },
    StopAll,
    ResumeAll,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeartbeatReply {
    /// Newly leased jobs to start now.
    pub assign: Vec<Job>,
    pub commands: Vec<AgentCommand>,
}

/// POST /v1/agent/job-update, on every status change of a leased job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobUpdate {
    /// Per-device sequence number from the agent outbox, unique across restarts. The
    /// coordinator applies each (device, seq) once; 0 means unsequenced (old agent).
    pub seq: u64,
    pub id: JobId,
    pub device: DeviceId,
    pub status: JobStatus,
    pub at_ms: UnixMs,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub output_path: Option<String>,
    /// AI job hit a usage limit or HTTP 429; blocks that account for the window.
    pub rate_limited: bool,
    /// Last 64 KiB of stdout, sent with the terminal status (advisor AI pass output).
    pub output_tail: Option<String>,
    /// A `killed` update caused by the owner (local stop_all): final, never re-queued.
    pub owner_stop: bool,
}

/// Reply to POST /v1/agent/job-update.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobUpdateReply {
    /// The update's `seq`, once it is stored (or was stored before). None from a
    /// coordinator that predates sequencing, or for an unsequenced update.
    pub acked_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceView {
    pub info: DeviceInfo,
    pub online: bool,
    pub last_seen_ms: UnixMs,
    pub metrics: Option<MetricsSnapshot>,
    pub guard: Option<GuardDecision>,
    pub running: Vec<RunningJob>,
    /// Paused by a coordinator control.
    pub paused: bool,
    /// Paused on the device itself (kill switch file or local control).
    pub local_paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobCounts {
    pub queued: u64,
    pub leased: u64,
    pub running: u64,
    pub paused: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub killed: u64,
    pub cancelled: u64,
    pub parked: u64,
}

impl JobCounts {
    pub fn add(&mut self, status: JobStatus, n: u64) {
        let slot = match status {
            JobStatus::Queued => &mut self.queued,
            JobStatus::Leased => &mut self.leased,
            JobStatus::Running => &mut self.running,
            JobStatus::Paused => &mut self.paused,
            JobStatus::Succeeded => &mut self.succeeded,
            JobStatus::Failed => &mut self.failed,
            JobStatus::Killed => &mut self.killed,
            JobStatus::Cancelled => &mut self.cancelled,
            JobStatus::Parked => &mut self.parked,
        };
        *slot += n;
    }
}

/// GET /v1/status.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterStatus {
    pub at_ms: UnixMs,
    pub version: String,
    pub global_paused: bool,
    pub paused_accounts: Vec<String>,
    pub devices: Vec<DeviceView>,
    pub jobs: JobCounts,
    pub open_advice: u64,
}

/// POST /v1/control and POST /v1/local/control (local accepts Global scope only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRequest {
    pub scope: ControlScope,
    pub action: ControlAction,
    #[serde(default)]
    pub reason: Option<String>,
}

/// GET /v1/local/status on the agent.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalStatus {
    pub device: DeviceId,
    pub at_ms: UnixMs,
    pub version: String,
    pub coordinator_reachable: bool,
    pub last_heartbeat_ms: Option<UnixMs>,
    /// Paused by POST /v1/local/control.
    pub local_paused: bool,
    /// `$IDLEMAX_HOME/PAUSE` exists.
    pub kill_switch: bool,
    pub guard: GuardDecision,
    pub metrics: Option<MetricsSnapshot>,
    pub running: Vec<RunningJob>,
}

/// Body of every non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

impl ApiError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { error: msg.into() }
    }
}
