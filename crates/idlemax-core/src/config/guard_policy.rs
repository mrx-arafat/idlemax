//! Domain: the guard thresholds, the two device presets and the `[guard]` overrides. Pure.

use serde::{Deserialize, Serialize};

use super::ConfigError;
use crate::types::{DeviceProfile, MemoryPressure};

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
    /// Load average outside the jobs, per CPU: pause above this.
    pub load_per_cpu_pause: Option<f64>,
    pub load_per_cpu_kill: Option<f64>,
    /// Absolute 1 minute load average, jobs included. Off in the presets: it does not scale
    /// with the number of CPUs; `load_per_cpu_*` does.
    pub load_1m_pause: Option<f64>,
    pub load_1m_kill: Option<f64>,
    /// Below this, start no new jobs (running ones go on: pausing frees no disk).
    pub disk_free_mib_pause: Option<u64>,
    /// Below this, kill running jobs. Measured on the least free of `/`, the state dir and the
    /// running jobs' working directories.
    pub disk_free_mib_kill: Option<u64>,
}

const GIB_MIB: u64 = 1024;

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
            load_per_cpu_pause: Some(1.0),
            load_per_cpu_kill: Some(2.0),
            load_1m_pause: None,
            load_1m_kill: None,
            disk_free_mib_pause: Some(2 * GIB_MIB),
            disk_free_mib_kill: Some(GIB_MIB),
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
            // Off: Linux keeps cold pages in swap for days, so swap in use is not pressure.
            // The swap-in rate, PSI and MemAvailable below measure pressure.
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
            disk_free_mib_pause: Some(2 * GIB_MIB),
            disk_free_mib_kill: Some(GIB_MIB),
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
