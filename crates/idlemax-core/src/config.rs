//! Domain: `config.toml` schema with full defaults, guard presets and token rules. Pure.

use serde::{Deserialize, Serialize};

use crate::quota::{QuotaConfig, QuotaConfigError};
use crate::types::{DeviceProfile, MemoryPressure, Os};
use crate::{DEFAULT_AGENT_LISTEN, DEFAULT_COORD_LISTEN, DEFAULT_COORD_URL};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("{0}")]
    Guard(String),
    #[error("[quota]: {0}")]
    Quota(#[from] QuotaConfigError),
    #[error("token is empty")]
    EmptyToken,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub coordinator: CoordinatorConfig,
    pub agent: AgentConfig,
    /// Field overrides on top of the device profile preset, e.g. `load_1m_pause = 4.0`.
    pub guard: toml::Table,
    pub quota: QuotaConfig,
    pub advisor: AdvisorConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoordinatorConfig {
    /// Where `idlemax coord` binds (the tailnet IP in production).
    pub listen: String,
    /// Where agents and the CLI reach the coordinator.
    pub url: String,
    pub lease_secs: u64,
    /// A device is offline after this long without a heartbeat.
    pub offline_after_secs: u64,
    pub metrics_retention_days: u32,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            listen: DEFAULT_COORD_LISTEN.into(),
            url: DEFAULT_COORD_URL.into(),
            lease_secs: 120,
            offline_after_secs: 90,
            metrics_retention_days: 7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Defaults to the hostname.
    pub device_id: Option<String>,
    /// Defaults from the OS (macos_laptop on macOS, linux_server elsewhere).
    pub profile: Option<DeviceProfile>,
    /// Local agent API bind address.
    pub listen: String,
    pub sample_secs: u64,
    pub heartbeat_secs: u64,
    /// Seconds between sends of the advisor inputs (`gh` summary, session metadata) to the
    /// coordinator. 0 sends nothing from this device.
    pub advisor_inputs_secs: u64,
    /// AI accounts this device may spend; names from `[[quota.accounts]]`.
    pub ai_accounts: Vec<String>,
    /// Allowed argv[0] values (bare names or absolute paths). Empty allows nothing.
    pub command_allowlist: Vec<String>,
    /// Send desktop notification on job finish (macOS only, default false).
    pub notify: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            device_id: None,
            profile: None,
            listen: DEFAULT_AGENT_LISTEN.into(),
            sample_secs: 30,
            heartbeat_secs: 30,
            advisor_inputs_secs: 15 * 60,
            ai_accounts: Vec::new(),
            command_allowlist: Vec::new(),
            notify: false,
        }
    }
}

impl AgentConfig {
    pub fn profile_or_default(&self, os: Os) -> DeviceProfile {
        self.profile
            .unwrap_or_else(|| DeviceProfile::default_for(os))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvisorConfig {
    pub enabled: bool,
    /// Account the AI pass spends; None runs the rules engine only.
    pub ai_account: Option<String>,
    /// Job names whose recommendations are accepted without a human.
    pub auto_accept: Vec<String>,
}

impl Default for AdvisorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ai_account: None,
            auto_accept: Vec::new(),
        }
    }
}

impl Config {
    /// Parses and validates `config.toml` text. Missing sections take defaults.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Checks `[guard]` overrides against both presets, and the quota accounts.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for os in [Os::Macos, Os::Linux] {
            self.guard_policy(os)?;
        }
        self.quota.validate()?;
        Ok(())
    }

    /// Profile preset with `[guard]` overrides applied.
    pub fn guard_policy(&self, os: Os) -> Result<GuardPolicy, ConfigError> {
        let preset = GuardPolicy::for_profile(self.agent.profile_or_default(os));
        preset.with_overrides(&self.guard)
    }
}

/// Swap growth threshold: more than `mib` within `window_secs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwapGrowth {
    pub mib: u64,
    pub window_secs: u64,
}

/// Guard thresholds. `None` disables a check. Pause fires on one breach,
/// kill on `kill_after_breaches` consecutive kill-level breaches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardPolicy {
    pub max_jobs: u32,
    /// Sum of running jobs' mem_mb must stay within this.
    pub ram_budget_mib: u64,
    pub kill_after_breaches: u32,
    /// Clear samples needed after a pause or kill before Run again.
    pub resume_after_clear: u32,
    /// Pause while the user was active within this many seconds.
    pub user_idle_min_secs: Option<u64>,
    pub require_ac_power: bool,
    pub min_battery_pct: Option<f64>,
    pub pause_on_thermal: bool,
    pub memory_pressure_pause: Option<MemoryPressure>,
    pub memory_pressure_kill: Option<MemoryPressure>,
    pub mem_free_pct_pause: Option<f64>,
    pub mem_free_pct_kill: Option<f64>,
    pub mem_available_mib_pause: Option<u64>,
    pub mem_available_mib_kill: Option<u64>,
    pub swap_growth_pause: Option<SwapGrowth>,
    pub swap_growth_kill: Option<SwapGrowth>,
    pub swap_used_mib_pause: Option<u64>,
    pub swap_used_mib_kill: Option<u64>,
    pub swap_in_pages_per_sec_pause: Option<f64>,
    pub swap_in_sustain_secs: u64,
    pub psi_mem_some_pause: Option<f64>,
    pub psi_mem_full_kill: Option<f64>,
    pub psi_io_full_pause: Option<f64>,
    pub psi_io_full_kill: Option<f64>,
    pub load_1m_pause: Option<f64>,
    pub load_1m_kill: Option<f64>,
    pub disk_free_mib_pause: Option<u64>,
    pub disk_free_mib_kill: Option<u64>,
}

const GIB_MIB: u64 = 1024;
/// 1 GB (10^9 bytes) in MiB, rounded up so the threshold is never looser.
const fn gb_mib(gb: u64) -> u64 {
    (gb * 1_000_000_000).div_ceil(1024 * 1024)
}

impl GuardPolicy {
    pub fn for_profile(profile: DeviceProfile) -> Self {
        match profile {
            DeviceProfile::MacosLaptop => Self::macos_laptop(),
            DeviceProfile::LinuxServer => Self::linux_server(),
        }
    }

    /// Numbers from the capacity research summarized in docs/PLAN.md (guard presets).
    pub fn macos_laptop() -> Self {
        Self {
            max_jobs: 2,
            ram_budget_mib: GIB_MIB,
            kill_after_breaches: 2,
            resume_after_clear: 2,
            user_idle_min_secs: Some(600),
            require_ac_power: true,
            min_battery_pct: Some(40.0),
            pause_on_thermal: true,
            memory_pressure_pause: Some(MemoryPressure::Warn),
            memory_pressure_kill: Some(MemoryPressure::Critical),
            mem_free_pct_pause: Some(30.0),
            mem_free_pct_kill: Some(20.0),
            mem_available_mib_pause: None,
            mem_available_mib_kill: None,
            swap_growth_pause: Some(SwapGrowth {
                mib: 256,
                window_secs: 60,
            }),
            swap_growth_kill: Some(SwapGrowth {
                mib: GIB_MIB,
                window_secs: 300,
            }),
            swap_used_mib_pause: None,
            swap_used_mib_kill: None,
            swap_in_pages_per_sec_pause: None,
            swap_in_sustain_secs: 60,
            psi_mem_some_pause: None,
            psi_mem_full_kill: None,
            psi_io_full_pause: None,
            psi_io_full_kill: None,
            load_1m_pause: Some(6.0),
            load_1m_kill: Some(8.0),
            disk_free_mib_pause: Some(15 * GIB_MIB),
            disk_free_mib_kill: Some(10 * GIB_MIB),
        }
    }

    pub fn linux_server() -> Self {
        Self {
            max_jobs: 1,
            ram_budget_mib: 512,
            kill_after_breaches: 2,
            resume_after_clear: 2,
            user_idle_min_secs: None,
            require_ac_power: false,
            min_battery_pct: None,
            pause_on_thermal: true,
            memory_pressure_pause: None,
            memory_pressure_kill: None,
            mem_free_pct_pause: None,
            mem_free_pct_kill: None,
            mem_available_mib_pause: Some(1024),
            mem_available_mib_kill: Some(768),
            swap_growth_pause: None,
            swap_growth_kill: None,
            swap_used_mib_pause: Some(1536),
            swap_used_mib_kill: Some(2 * GIB_MIB),
            swap_in_pages_per_sec_pause: Some(100.0),
            swap_in_sustain_secs: 60,
            psi_mem_some_pause: Some(5.0),
            psi_mem_full_kill: Some(1.0),
            psi_io_full_pause: Some(10.0),
            psi_io_full_kill: Some(25.0),
            load_1m_pause: Some(1.5),
            load_1m_kill: Some(2.0),
            disk_free_mib_pause: Some(gb_mib(10)),
            disk_free_mib_kill: Some(gb_mib(5)),
        }
    }

    /// Overlays `[guard]` keys; unknown keys or wrong types are errors.
    pub fn with_overrides(&self, overrides: &toml::Table) -> Result<Self, ConfigError> {
        if overrides.is_empty() {
            return Ok(self.clone());
        }
        let mut table =
            toml::Table::try_from(self).map_err(|e| ConfigError::Guard(format!("[guard]: {e}")))?;
        for (k, v) in overrides {
            table.insert(k.clone(), v.clone());
        }
        toml::Value::Table(table)
            .try_into()
            .map_err(|e: toml::de::Error| guard_error(&e))
    }
}

/// toml words a bad value as "<message>\nin `<key>`\n"; this writes it on one line as
/// "guard.<key>: <message>" so the key path leads.
fn guard_error(e: &toml::de::Error) -> ConfigError {
    let shown = e.to_string();
    let shown = shown.trim_end();
    ConfigError::Guard(match shown.rsplit_once("\nin `") {
        Some((message, key)) => format!("guard.{}: {message}", key.trim_end_matches('`')),
        None => format!("[guard]: {shown}"),
    })
}

/// Token text from the env var or the token file, trimmed; empty is an error.
pub fn parse_token(text: &str) -> Result<String, ConfigError> {
    let token = text.trim();
    if token.is_empty() {
        return Err(ConfigError::EmptyToken);
    }
    Ok(token.to_string())
}

/// A token file is private when group and others have no access (mode & 0o077 == 0).
pub fn token_mode_is_private(mode: u32) -> bool {
    mode & 0o077 == 0
}

/// Lowercase hex of random bytes the adapter read from the OS RNG.
pub fn encode_token(random: &[u8]) -> String {
    random.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_config_notify_flag() {
        let default_cfg: Config = toml::from_str("").unwrap();
        assert!(!default_cfg.agent.notify);

        let custom_cfg: Config = toml::from_str("[agent]\nnotify = true\n").unwrap();
        assert!(custom_cfg.agent.notify);
    }

    #[test]
    fn a_bad_guard_value_is_one_line_naming_its_key() {
        let err = Config::parse("[guard]\nswap_growth_kill = 100000000000\n").unwrap_err();
        let msg = err.to_string();
        assert_eq!(msg.lines().count(), 1, "{msg:?}");
        assert!(
            msg.starts_with("guard.swap_growth_kill: invalid type"),
            "{msg:?}"
        );
    }
}
