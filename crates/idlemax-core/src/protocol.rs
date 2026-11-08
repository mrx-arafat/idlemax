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

/// Longest device id and host name an agent may report.
pub const MAX_DEVICE_ID_CHARS: usize = 128;
pub const MAX_HOSTNAME_CHARS: usize = 253;
/// Entries in a device's capability and AI account lists, the length of each, and the entries
/// of its reported command allowlist.
const MAX_TAGS: usize = 64;
const MAX_TAG_CHARS: usize = 128;
const MAX_ALLOWLIST_ENTRIES: usize = 256;

/// Why `value` cannot be a `field` (control characters, or over `max` characters), or None.
/// Names reach events, the CLI tables and the AI prompt, so they stay plain and short.
pub fn name_problem(field: &str, value: &str, max: usize) -> Option<String> {
    if value.chars().any(char::is_control) {
        return Some(format!("{field} contains a control character"));
    }
    (value.chars().count() > max).then(|| format!("{field} is longer than {max} characters"))
}

impl Heartbeat {
    /// The first reason this heartbeat must be refused for its size or characters, if any.
    pub fn check(&self) -> Result<(), String> {
        let d = &self.device;
        let checks = [
            name_problem("device.id", &d.id, MAX_DEVICE_ID_CHARS),
            name_problem("device.hostname", &d.hostname, MAX_HOSTNAME_CHARS),
            name_problem("device.agent_version", &d.agent_version, MAX_TAG_CHARS),
        ];
        if let Some(problem) = checks.into_iter().flatten().next() {
            return Err(problem);
        }
        for (field, list) in [
            ("device.capabilities", &d.capabilities),
            ("device.ai_accounts", &d.ai_accounts),
        ] {
            if list.len() > MAX_TAGS {
                return Err(format!("{field} has more than {MAX_TAGS} entries"));
            }
            if let Some(problem) = list
                .iter()
                .find_map(|t| name_problem(field, t, MAX_TAG_CHARS))
            {
                return Err(problem);
            }
        }
        match &d.command_allowlist {
            Some(list) if list.len() > MAX_ALLOWLIST_ENTRIES => Err(format!(
                "device.command_allowlist has more than {MAX_ALLOWLIST_ENTRIES} entries"
            )),
            _ => Ok(()),
        }
    }
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
    /// The lease count of the attempt this update belongs to (`Job.attempts` when it was
    /// leased). With it the coordinator can still apply a result that arrives after the
    /// lease expired; 0 (an old agent, or an orphan reaped after a restart) never matches.
    pub attempt: u32,
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
    /// The attempt ended for a reason that is not the job's fault (the guard killed it, the
    /// agent shut down or restarted, or the agent could not start it), so the coordinator
    /// may run the job again. Every other `failed` or `killed` update is final, and so is
    /// one from an agent that predates the field.
    pub retryable: bool,
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
