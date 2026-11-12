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
/// Site reserve target; the legacy budget takes precedence on small hosts.
const SERVER_CPU_RESERVE: u32 = 1;
pub(crate) const SERVER_RAM_RESERVE_MIB: u64 = 1536;
const WORKBOX_CPU_RESERVE: u32 = 1;
pub(crate) const WORKBOX_RAM_RESERVE_MIB: u64 = 1024;
/// Room for the agent itself in addition to its jobs.
pub const AGENT_OVERHEAD_MIB: u64 = 128;

/// Unlimited is a successfully read hierarchy, unlike an undetermined limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCap {
    Unknown,
    Unlimited,
    Limited(u64),
}

impl GuardPolicy {
    pub fn for_profile(profile: DeviceProfile) -> Self {
        match profile {
            DeviceProfile::Laptop => Self::laptop(),
            DeviceProfile::Workbox => Self::workbox(),
            DeviceProfile::Production => Self::production(),
        }
    }

    /// Numbers from the capacity research summarized in docs/PLAN.md (guard presets).
    pub fn laptop() -> Self {
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

    pub fn production() -> Self {
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

    pub fn workbox() -> Self {
        let mut policy = Self::production();
        policy.load_per_cpu_pause = None;
        policy.load_per_cpu_kill = None;
        policy.psi_mem_some_pause = Some(10.0);
        policy.psi_mem_full_kill = Some(5.0);
        policy
    }

    /// Raises the job slots and RAM budget to what a bigger machine can take: half its CPUs
    /// and a quarter of its RAM. The preset is the floor, and a key set in `overrides` wins.
    pub fn sized_for(
        mut self,
        cpu_count: u32,
        mem_total_bytes: u64,
        overrides: &toml::Table,
    ) -> Self {
        if !overrides.contains_key("max_jobs") {
            self.max_jobs = self.max_jobs.max(cpu_count / 2);
        }
        if !overrides.contains_key("ram_budget_mib") {
            let quarter = mem_total_bytes / 4 / (1024 * 1024);
            self.ram_budget_mib = self.ram_budget_mib.max(quarter);
        }
        self
    }

    /// Target a site reserve and quarter-RAM budget, retaining the historical 512-MiB
    /// floor so default jobs fit small hosts. Slots are bounded by both CPU and RAM.
    pub fn server_sized_for(mut self, cpus: u32, bytes: u64, overrides: &toml::Table) -> Self {
        if !overrides.contains_key("ram_budget_mib") {
            let total = bytes / (1024 * 1024);
            self.ram_budget_mib = if total == 0 {
                512
            } else {
                (total / 4)
                    .min(total.saturating_sub(SERVER_RAM_RESERVE_MIB + AGENT_OVERHEAD_MIB))
                    .max(512)
            };
        }
        if !overrides.contains_key("max_jobs") {
            let memory_slots = u32::try_from(self.ram_budget_mib / crate::types::DEFAULT_MEM_MB)
                .unwrap_or(u32::MAX)
                .max(1);
            self.max_jobs = cpus
                .saturating_sub(SERVER_CPU_RESERVE)
                .max(1)
                .min(memory_slots);
        }
        self
    }

    /// Gives plain jobs all the memory but a reserve of 1024 MiB and the agent's own 128 MiB,
    /// never under 512 MiB, and all CPUs but one, bounded by the memory slots.
    pub fn workbox_sized_for(mut self, cpus: u32, bytes: u64, overrides: &toml::Table) -> Self {
        if !overrides.contains_key("ram_budget_mib") {
            let total = bytes / (1024 * 1024);
            self.ram_budget_mib = if total == 0 {
                512
            } else {
                total
                    .saturating_sub(WORKBOX_RAM_RESERVE_MIB + AGENT_OVERHEAD_MIB)
                    .max(512)
            };
        }
        if !overrides.contains_key("max_jobs") {
            let memory_slots = u32::try_from(self.ram_budget_mib / crate::types::DEFAULT_MEM_MB)
                .unwrap_or(u32::MAX)
                .max(1);
            self.max_jobs = cpus
                .saturating_sub(WORKBOX_CPU_RESERVE)
                .max(1)
                .min(memory_slots);
        }
        self
    }

    pub fn apply_memory_cap(
        &mut self,
        cap: MemoryCap,
        server: bool,
    ) -> Option<crate::types::MemoryLimit> {
        match cap {
            MemoryCap::Limited(bytes) => self.clamp_memory(Some(bytes)),
            MemoryCap::Unlimited => None,
            MemoryCap::Unknown if server => {
                let configured_mib = self.ram_budget_mib;
                self.max_jobs = self.max_jobs.min(1);
                self.ram_budget_mib = self.ram_budget_mib.min(512);
                Some(crate::types::MemoryLimit {
                    configured_mib,
                    effective_mib: self.ram_budget_mib,
                    cgroup_max_bytes: None,
                })
            }
            MemoryCap::Unknown => None,
        }
    }

    /// A cgroup bounds the agent and its jobs together, including explicitly pinned budgets.
    pub fn clamp_memory(&mut self, cap_bytes: Option<u64>) -> Option<crate::types::MemoryLimit> {
        let cap_bytes = cap_bytes?;
        let available = (cap_bytes / (1024 * 1024)).saturating_sub(AGENT_OVERHEAD_MIB);
        if self.ram_budget_mib <= available {
            return None;
        }
        let limit = crate::types::MemoryLimit {
            configured_mib: self.ram_budget_mib,
            effective_mib: available,
            cgroup_max_bytes: Some(cap_bytes),
        };
        self.ram_budget_mib = available;
        Some(limit)
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
