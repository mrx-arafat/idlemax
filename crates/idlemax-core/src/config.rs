//! Domain: `config.toml` schema with full defaults, guard presets and token rules. Pure.

use serde::{Deserialize, Serialize};

use crate::quota::{QuotaConfig, QuotaConfigError};
use crate::types::{DeviceProfile, Os};
use crate::{DEFAULT_AGENT_LISTEN, DEFAULT_COORD_LISTEN, DEFAULT_COORD_URL};

mod dispatcher;
mod edit;
mod guard_policy;
mod limits;
mod reload;
mod settings;
mod workers;

pub use dispatcher::DispatcherConfig;
pub use edit::{find_setting, set_setting, unset_setting, Edit, EditError};
pub use guard_policy::{GuardPolicy, MemoryCap, SwapGrowth, AGENT_OVERHEAD_MIB};
pub use limits::{limit_settings, set_limit, LimitError, LimitReply, LimitRequest, OWNER_LIMITS};
pub use reload::{plan_reload, ReloadReport};
pub use settings::{list_settings, show_value, Setting};
pub use workers::WorkersConfig;

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
    pub dispatcher: DispatcherConfig,
    pub workers: WorkersConfig,
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
    /// Defaults from the OS (laptop on macOS, production elsewhere).
    pub profile: Option<DeviceProfile>,
    /// Local agent API bind address.
    pub listen: String,
    pub sample_secs: u64,
    pub heartbeat_secs: u64,
    /// Seconds between sends of the advisor inputs (`gh` summary, session metadata) to the
    /// coordinator. 0 sends nothing from this device.
    pub advisor_inputs_secs: u64,
    /// Seconds between sends of the dispatch inputs (repositories, owner activity, roadmap
    /// items) to the coordinator. 0 sends nothing from this device.
    pub dispatch_inputs_secs: u64,
    /// AI accounts this device may spend; names from `[[quota.accounts]]`.
    pub ai_accounts: Vec<String>,
    /// Allowed argv[0] values (bare names or absolute paths). Empty allows nothing.
    pub command_allowlist: Vec<String>,
    /// Send desktop notification on job finish (macOS only, default false).
    pub notify: bool,
    /// Prevent idle system sleep on AC while this device has useful work (macOS).
    pub keep_awake: crate::keep_awake::KeepAwake,
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
            dispatch_inputs_secs: 60,
            ai_accounts: Vec::new(),
            command_allowlist: Vec::new(),
            notify: false,
            keep_awake: crate::keep_awake::KeepAwake::Off,
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

    /// Checks `[guard]` overrides against both presets, the quota accounts, the
    /// dispatcher lines and the worker settings.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for os in [Os::Macos, Os::Linux] {
            self.guard_policy(os)?;
        }
        self.quota.validate()?;
        self.dispatcher.validate().map_err(ConfigError::Guard)?;
        self.workers.validate().map_err(ConfigError::Guard)?;
        Ok(())
    }

    /// Profile preset with `[guard]` overrides applied.
    pub fn guard_policy(&self, os: Os) -> Result<GuardPolicy, ConfigError> {
        let preset = GuardPolicy::for_profile(self.agent.profile_or_default(os));
        preset.with_overrides(&self.guard)
    }

    /// `guard_policy` with unset capacity sized to this host and the profile's reserve.
    pub fn guard_policy_for(
        &self,
        os: Os,
        cpu_count: u32,
        mem_total_bytes: u64,
    ) -> Result<GuardPolicy, ConfigError> {
        let policy = self.guard_policy(os)?;
        Ok(match self.agent.profile_or_default(os) {
            DeviceProfile::Laptop => policy.sized_for(cpu_count, mem_total_bytes, &self.guard),
            DeviceProfile::Workbox => {
                policy.workbox_sized_for(cpu_count, mem_total_bytes, &self.guard)
            }
            DeviceProfile::Production => {
                policy.server_sized_for(cpu_count, mem_total_bytes, &self.guard)
            }
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
    }

    #[test]
    fn server_capacity_reserves_resources_and_honours_explicit_overrides() {
        let server = "[agent]\nprofile = \"linux_server\"\n";
        let cfg = Config::parse(server).unwrap();
        for (cpus, gib, jobs, ram) in [
            (2, 2, 1, 512),
            (4, 4, 2, 1024),
            (8, 8, 4, 2048),
            (16, 16, 8, 4096),
        ] {
            let policy = cfg.guard_policy_for(Os::Linux, cpus, gib << 30).unwrap();
            assert_eq!((policy.max_jobs, policy.ram_budget_mib), (jobs, ram));
        }
        let small = cfg.guard_policy_for(Os::Linux, 4, 1950 << 20).unwrap();
        assert_eq!((small.max_jobs, small.ram_budget_mib), (1, 512));
        let cfg = Config::parse(&format!(
            "{server}[guard]\nmax_jobs = 1\nram_budget_mib = 256\n"
        ))
        .unwrap();
        let policy = cfg.guard_policy_for(Os::Linux, 16, 16 << 30).unwrap();
        assert_eq!((policy.max_jobs, policy.ram_budget_mib), (1, 256));
    }

    #[test]
    fn cgroup_memory_clamps_even_pinned_budgets_and_reserves_agent_overhead() {
        let mut unknown = Config::default()
            .guard_policy_for(Os::Linux, 16, 16 << 30)
            .unwrap();
        let note = unknown.apply_memory_cap(MemoryCap::Unknown, true).unwrap();
        assert_eq!((unknown.max_jobs, unknown.ram_budget_mib), (1, 512));
        assert!(note.to_string().contains("reference fallback"));
        assert!(note.cgroup_max_bytes.is_none());
        for (cap, budget, expected) in [
            (None, 4096, 4096),
            (Some(640 << 20), 4096, 512),
            (Some(8192 << 20), 4096, 4096),
            (Some(127 << 20), 512, 0),
            (Some((640 << 20) + 1), 4096, 512),
        ] {
            let mut policy = GuardPolicy::production();
            policy.ram_budget_mib = budget;
            let note = policy.clamp_memory(cap);
            assert_eq!(policy.ram_budget_mib, expected);
            assert_eq!(note.is_some(), budget != expected);
        }
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

    #[test]
    fn profiles_parse_aliases_defaults_and_sizing() {
        const GIB: u64 = 1 << 30;
        let linux_old = Config::parse("[agent]\nprofile = \"linux_server\"\n").unwrap();
        assert_eq!(linux_old.agent.profile, Some(DeviceProfile::Production));
        let mac_old = Config::parse("[agent]\nprofile = \"macos_laptop\"\n").unwrap();
        assert_eq!(mac_old.agent.profile, Some(DeviceProfile::Laptop));
        let prod_new = Config::parse("[agent]\nprofile = \"production\"\n").unwrap();
        assert_eq!(prod_new.agent.profile, Some(DeviceProfile::Production));
        let laptop_new = Config::parse("[agent]\nprofile = \"laptop\"\n").unwrap();
        assert_eq!(laptop_new.agent.profile, Some(DeviceProfile::Laptop));
        let workbox = Config::parse("[agent]\nprofile = \"workbox\"\n").unwrap();
        assert_eq!(workbox.agent.profile, Some(DeviceProfile::Workbox));

        assert!(prod_new
            .to_toml_string()
            .unwrap()
            .contains("profile = \"production\""));
        assert!(laptop_new
            .to_toml_string()
            .unwrap()
            .contains("profile = \"laptop\""));
        assert!(workbox
            .to_toml_string()
            .unwrap()
            .contains("profile = \"workbox\""));

        assert_eq!(
            DeviceProfile::default_for(Os::Linux),
            DeviceProfile::Production
        );
        assert_eq!(DeviceProfile::default_for(Os::Macos), DeviceProfile::Laptop);

        let workbox_policy = workbox.guard_policy_for(Os::Linux, 8, 16 * GIB).unwrap();
        assert_eq!(
            (workbox_policy.max_jobs, workbox_policy.ram_budget_mib),
            (7, 15232)
        );
        let prod_policy = prod_new.guard_policy_for(Os::Linux, 8, 16 * GIB).unwrap();
        assert_eq!(
            (prod_policy.max_jobs, prod_policy.ram_budget_mib),
            (7, 4096)
        );
        let small_workbox = workbox.guard_policy_for(Os::Linux, 2, 2 * GIB).unwrap();
        assert_eq!(
            (small_workbox.max_jobs, small_workbox.ram_budget_mib),
            (1, 896)
        );

        let wb_preset = GuardPolicy::workbox();
        assert_eq!(wb_preset.psi_mem_some_pause, Some(10.0));
        assert_eq!(wb_preset.psi_mem_full_kill, Some(5.0));
        assert_eq!(wb_preset.load_per_cpu_pause, None);
        assert_eq!(wb_preset.load_per_cpu_kill, None);
        // The numbers `linux_server()` had before the rename.
        let old_server = GuardPolicy {
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
            swap_used_mib_pause: None,
            swap_used_mib_kill: None,
            swap_in_pages_per_sec_pause: Some(100.0),
            swap_in_sustain_secs: 60,
            psi_mem_some_pause: Some(5.0),
            psi_mem_full_kill: Some(1.0),
            psi_io_full_pause: Some(10.0),
            psi_io_full_kill: Some(25.0),
            load_per_cpu_pause: Some(0.75),
            load_per_cpu_kill: Some(1.5),
            load_1m_pause: None,
            load_1m_kill: None,
            disk_free_mib_pause: Some(2048),
            disk_free_mib_kill: Some(1024),
        };
        assert_eq!(GuardPolicy::production(), old_server);
    }
}
