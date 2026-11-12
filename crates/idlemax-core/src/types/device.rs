//! Domain: device identity and health samples (OS, guard profile, metrics, device info).

use serde::{Deserialize, Serialize};

use super::{DeviceId, UnixMs};

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
    #[serde(alias = "macos_laptop")]
    Laptop,
    Workbox,
    #[serde(alias = "linux_server")]
    Production,
}

impl DeviceProfile {
    // SHORTCUT: Windows maps to production, add a windows_desktop preset when a Windows device joins.
    pub fn default_for(os: Os) -> Self {
        match os {
            Os::Macos => DeviceProfile::Laptop,
            _ => DeviceProfile::Production,
        }
    }

    /// The name `config.toml` and `--profile` use.
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceProfile::Laptop => "laptop",
            DeviceProfile::Workbox => "workbox",
            DeviceProfile::Production => "production",
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
    /// App, wired and compressed memory over the total, as Activity Monitor shows it; None
    /// where it is not read.
    pub mem_used_pct: Option<f64>,
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
    /// Claude login reported by the agent. None means the agent did not ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_login: Option<crate::quota::login::Login>,
}
