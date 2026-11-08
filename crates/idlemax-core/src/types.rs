//! Domain: shared contract types (devices, metrics, jobs, guard, control, quota, events, advice).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type DeviceId = String;
pub type JobId = i64;
/// Unix time in milliseconds. Domain code takes it as a parameter; adapters read the
/// clock with `crate::state::now_ms` in the bin crate.
pub type UnixMs = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Macos,
    Linux,
    Windows,
    #[default]
    Other,
}

impl Os {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Os::Macos,
            "linux" => Os::Linux,
            "windows" => Os::Windows,
            _ => Os::Other,
        }
    }
}

/// Guard preset family. Servers take only CpuLight and Io jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceProfile {
    #[default]
    MacosLaptop,
    LinuxServer,
}

impl DeviceProfile {
    // SHORTCUT: Windows maps to linux_server, add a windows_desktop preset when a Windows device joins.
    pub fn default_for(os: Os) -> Self {
        match os {
            Os::Macos => DeviceProfile::MacosLaptop,
            _ => DeviceProfile::LinuxServer,
        }
    }
}

/// macOS `memory_pressure` level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Warn,
    Critical,
}

/// One sample of device health. `None` means the signal is not available on this OS.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsSnapshot {
    pub at_ms: UnixMs,
    pub cpu_count: u32,
    pub cpu_busy_pct: f64,
    pub load_avg_1m: f64,
    pub mem_total_bytes: u64,
    pub mem_available_bytes: u64,
    pub mem_free_pct: f64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub disk_free_bytes: u64,
    pub disk_total_bytes: u64,
    /// Seconds since last keyboard or mouse input (macOS HIDIdleTime, logind).
    pub user_idle_secs: Option<u64>,
    pub on_ac_power: Option<bool>,
    pub battery_pct: Option<f64>,
    /// macOS CPU_Speed_Limit < 100, or a Linux thermal trip point reached.
    pub thermal_limited: Option<bool>,
    pub memory_pressure: Option<MemoryPressure>,
    pub psi_mem_some_avg10: Option<f64>,
    pub psi_mem_full_avg10: Option<f64>,
    pub psi_io_full_avg10: Option<f64>,
    /// Pages swapped in per second since the previous sample.
    pub swap_in_per_sec: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub hostname: String,
    pub os: Os,
    pub arch: String,
    pub cpu_count: u32,
    pub mem_total_bytes: u64,
    /// Tools found on PATH, e.g. "git", "cargo", "claude", "codex".
    pub capabilities: Vec<String>,
    /// AI account names this device may spend (config `agent.ai_accounts`).
    pub ai_accounts: Vec<String>,
    pub agent_version: String,
    pub profile: DeviceProfile,
    /// Config `agent.command_allowlist`: the only argv[0] values this device runs.
    /// None means the agent predates the field, so the coordinator cannot tell.
    pub command_allowlist: Option<Vec<String>>,
}

/// What a job does when the user becomes active on its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivePolicy {
    #[default]
    Pause,
    Continue,
    Kill,
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

/// A job request. `command` is argv, never run through a shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobSpec {
    pub name: String,
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
}

impl Default for JobSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
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
            (CostClass::Ai, None) => Err(SpecError::AiWithoutAccount),
            (c, Some(_)) if c != CostClass::Ai => Err(SpecError::AccountWithoutAi),
            _ => Ok(()),
        }
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

/// Guard verdict, ordered by severity: Run < Hold < Pause < Kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuardAction {
    /// Running jobs may run (paused ones resume). Up to `slots` more jobs may start
    /// with at most `mem_mib` of `mem_mb` between them.
    Run { slots: u32, mem_mib: u64 },
    /// Start nothing new; leave running and paused jobs as they are.
    #[default]
    Hold,
    /// SIGSTOP running jobs; start nothing.
    Pause,
    /// SIGTERM then SIGKILL running jobs; start nothing.
    Kill,
}

impl GuardAction {
    pub fn severity(self) -> u8 {
        match self {
            GuardAction::Run { .. } => 0,
            GuardAction::Hold => 1,
            GuardAction::Pause => 2,
            GuardAction::Kill => 3,
        }
    }

    /// The more severe of two actions; on a tie between two Runs, the smaller one.
    pub fn max(self, other: GuardAction) -> GuardAction {
        match (self, other) {
            (
                GuardAction::Run {
                    slots: a,
                    mem_mib: m,
                },
                GuardAction::Run {
                    slots: b,
                    mem_mib: n,
                },
            ) => GuardAction::Run {
                slots: a.min(b),
                mem_mib: m.min(n),
            },
            _ if other.severity() > self.severity() => other,
            _ => self,
        }
    }

    pub fn may_start(self) -> bool {
        matches!(self, GuardAction::Run { slots, mem_mib } if slots > 0 && mem_mib > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardDecision {
    /// Combined verdict: use for starting jobs and for jobs with on_user_active = pause.
    pub action: GuardAction,
    /// Verdict from machine health and the kill switch only, ignoring user presence.
    /// Jobs with on_user_active = continue follow this; on_user_active = kill jobs are
    /// killed when `user_active` is true.
    pub resource_action: GuardAction,
    pub user_active: bool,
    pub reasons: Vec<String>,
    pub at_ms: UnixMs,
}

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
