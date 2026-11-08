//! Domain: `config.toml` schema with full defaults, guard presets and token rules. Pure.

use serde::{Deserialize, Serialize};

use crate::quota::{QuotaConfig, QuotaConfigError};
use crate::types::{DeviceProfile, Os};
use crate::{DEFAULT_AGENT_LISTEN, DEFAULT_COORD_LISTEN, DEFAULT_COORD_URL};

mod guard_policy;

pub use guard_policy::{GuardPolicy, SwapGrowth};

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
    #[error("token is too short ({len} of at least {MIN_TOKEN_CHARS} characters; `idlemax init` makes 64)")]
    WeakToken { len: usize },
}

/// Shortest token accepted. The token is the only thing between the network and a command
/// runner, and `idlemax init` generates 64 characters (32 random bytes as hex).
pub const MIN_TOKEN_CHARS: usize = 32;

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
    /// Host names (besides IP addresses, `localhost` and the host of `url`) the coordinator
    /// answers to, such as a MagicDNS name. Any other name in a `Host` header gets 421.
    pub allowed_hosts: Vec<String>,
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
            allowed_hosts: Vec::new(),
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

    /// `guard_policy` with the laptop's job slots and RAM budget sized to this machine. The
    /// server stays at its preset: the packaged systemd unit caps the agent and its job.
    pub fn guard_policy_for(
        &self,
        os: Os,
        cpu_count: u32,
        mem_total_bytes: u64,
    ) -> Result<GuardPolicy, ConfigError> {
        let policy = self.guard_policy(os)?;
        Ok(match self.agent.profile_or_default(os) {
            DeviceProfile::MacosLaptop => policy.sized_for(cpu_count, mem_total_bytes, &self.guard),
            DeviceProfile::LinuxServer => policy,
        })
    }
}

/// Token text from the env var or the token file, trimmed; empty or shorter than
/// `MIN_TOKEN_CHARS` is an error.
pub fn parse_token(text: &str) -> Result<String, ConfigError> {
    let token = text.trim();
    if token.is_empty() {
        return Err(ConfigError::EmptyToken);
    }
    let len = token.chars().count();
    if len < MIN_TOKEN_CHARS {
        return Err(ConfigError::WeakToken { len });
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
    fn a_token_must_be_long_enough_to_resist_guessing() {
        assert!(matches!(parse_token(" \n"), Err(ConfigError::EmptyToken)));
        assert!(matches!(
            parse_token("a\n"),
            Err(ConfigError::WeakToken { len: 1 })
        ));
        let short = "x".repeat(MIN_TOKEN_CHARS - 1);
        assert!(matches!(
            parse_token(&short),
            Err(ConfigError::WeakToken { len }) if len == MIN_TOKEN_CHARS - 1
        ));
        let enough = "x".repeat(MIN_TOKEN_CHARS);
        assert_eq!(parse_token(&format!("  {enough}\n")).unwrap(), enough);
        // What `idlemax init` generates is accepted.
        assert!(parse_token(&encode_token(&[0xab; 32])).is_ok());
    }

    #[test]
    fn a_laptop_takes_half_the_cpus_and_a_quarter_of_the_ram_unless_the_config_sets_them() {
        const GIB: u64 = 1 << 30;
        let slots_and_ram = |text: &str, cpus: u32, ram: u64| {
            let config = Config::parse(text).unwrap();
            let p = config.guard_policy_for(Os::Macos, cpus, ram).unwrap();
            (p.max_jobs, p.ram_budget_mib)
        };
        let laptop = "[agent]\nprofile = \"macos_laptop\"\n";
        assert_eq!(slots_and_ram(laptop, 10, 16 * GIB), (5, 4096));
        // A small machine keeps the preset: 2 jobs, 1 GiB.
        assert_eq!(slots_and_ram(laptop, 2, 2 * GIB), (2, 1024));
        let pinned = format!("{laptop}[guard]\nmax_jobs = 1\nram_budget_mib = 256\n");
        assert_eq!(slots_and_ram(&pinned, 10, 16 * GIB), (1, 256));
        let server = "[agent]\nprofile = \"linux_server\"\n";
        assert_eq!(slots_and_ram(server, 6, 16 * GIB), (1, 512));
    }

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
