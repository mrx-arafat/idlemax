//! Adapter: reads device health into the domain `MetricsSnapshot` and `DeviceInfo`.
//!
//! `Sampler::sample()` is blocking (a few short probe commands, each capped by a
//! timeout, normally ~30 ms total). From async code call it via `spawn_blocking`.
//! It never fails: a signal that cannot be read is `None`, or 0 for the required
//! numbers, which makes the guard fail safe (pause) rather than open.

mod caps;
mod cmd;
pub mod disk;
mod linux;
mod macos;
mod macos_vm;

use std::path::PathBuf;
use std::time::Instant;

use idlemax_core::config::AgentConfig;
use idlemax_core::types::{DeviceInfo, MemoryPressure, MetricsSnapshot, Os};
use sysinfo::System;

use crate::state::now_ms;

/// Power source and charge, shared by the macOS and Linux probes.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Power {
    pub on_ac: Option<bool>,
    pub battery_pct: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("metrics sampler init failed: {0}")]
    Init(#[source] sysinfo::Error),
}

/// Last swap-in counter (Linux pswpin, macOS Swapins) and when it was read.
type SwapInMark = Option<(u64, Instant)>;

pub struct Sampler {
    sys: System,
    cpu_count: u32,
    #[cfg(target_os = "linux")]
    prev_pswpin: SwapInMark,
    #[cfg(target_os = "macos")]
    prev_therm: Option<(Option<bool>, Instant)>,
    #[cfg(target_os = "macos")]
    prev_swapins: SwapInMark,
}

impl Sampler {
    /// Primes CPU usage so the first `sample()` is accurate (blocks ~200 ms).
    pub fn new() -> Result<Self, MetricsError> {
        let mut sys = System::new().map_err(MetricsError::Init)?;
        sys.refresh_cpu_usage();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let cpu_count = sys.cpus().len() as u32;
        Ok(Self {
            sys,
            cpu_count,
            #[cfg(target_os = "linux")]
            prev_pswpin: None,
            #[cfg(target_os = "macos")]
            prev_therm: None,
            #[cfg(target_os = "macos")]
            prev_swapins: None,
        })
    }

    /// One snapshot. CPU busy % is the average since the previous call. Disk space is the
    /// least free of the filesystems holding `disk_paths`.
    pub fn sample(&mut self, disk_paths: &[PathBuf]) -> MetricsSnapshot {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let mem_total = self.sys.total_memory();
        let mem_avail = self.sys.available_memory();
        let (disk_free, disk_total) = disk::read_lowest(disk_paths).unwrap_or((0, 0));
        let mut s = MetricsSnapshot {
            at_ms: now_ms(),
            cpu_count: self.cpu_count,
            cpu_busy_pct: f64::from(self.sys.global_cpu_usage()).clamp(0.0, 100.0),
            load_avg_1m: System::load_average().map(|l| l.one).unwrap_or(0.0),
            mem_total_bytes: mem_total,
            mem_available_bytes: mem_avail,
            mem_free_pct: pct(mem_avail, mem_total),
            swap_total_bytes: self.sys.total_swap(),
            swap_used_bytes: self.sys.used_swap(),
            disk_free_bytes: disk_free,
            disk_total_bytes: disk_total,
            ..Default::default()
        };
        self.fill_os_signals(&mut s);
        s
    }

    #[cfg(target_os = "macos")]
    fn fill_os_signals(&mut self, s: &mut MetricsSnapshot) {
        let now = Instant::now();
        let cached_therm = self.prev_therm.and_then(|(v, at)| {
            (now.duration_since(at) < std::time::Duration::from_secs(60)).then_some(v)
        });
        let m = macos::read_with_cache(cached_therm);
        if cached_therm.is_none() {
            self.prev_therm = Some((m.thermal_limited, now));
        }
        s.user_idle_secs = m.user_idle_secs;
        s.on_ac_power = m.power.on_ac;
        s.battery_pct = m.power.battery_pct;
        s.thermal_limited = m.thermal_limited;
        // Same number `memory_pressure` prints; the macOS guard thresholds use it.
        if let Some(p) = m.mem_free_pct {
            s.mem_free_pct = p;
        }
        s.memory_pressure = m.vm_pressure_level.and_then(pressure_from_level);
        let vm = macos_vm::read();
        macos_vm::apply(s, vm.as_ref(), &mut self.prev_swapins, now);
    }

    #[cfg(target_os = "linux")]
    fn fill_os_signals(&mut self, s: &mut MetricsSnapshot) {
        let root = std::path::Path::new("/");
        let now_us = u64::try_from(s.at_ms).unwrap_or(0).saturating_mul(1000);
        let l = linux::read(root, linux::run_logind_seat(root).as_deref(), now_us);
        apply_linux(s, &l, &mut self.prev_pswpin, Instant::now());
    }

    // SHORTCUT: Windows reports no idle, power or thermal signals; add GetLastInputInfo
    // and GetSystemPowerStatus when a Windows device joins.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn fill_os_signals(&mut self, _s: &mut MetricsSnapshot) {}

    /// Static device description. Id is `agent.device_id`, else the short hostname.
    pub fn device_info(&mut self, cfg: &AgentConfig) -> DeviceInfo {
        self.sys.refresh_memory();
        let hostname = System::host_name().unwrap_or_default();
        let os = Os::current();
        DeviceInfo {
            id: cfg
                .device_id
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| default_device_id(&hostname)),
            hostname,
            os,
            arch: System::cpu_arch(),
            cpu_count: self.sys.cpus().len() as u32,
            mem_total_bytes: self.sys.total_memory(),
            capabilities: caps::detect(),
            ai_accounts: cfg.ai_accounts.clone(),
            agent_version: idlemax_core::VERSION.to_string(),
            profile: cfg.profile_or_default(os),
            command_allowlist: Some(cfg.command_allowlist.clone()),
        }
    }
}

/// First hostname label, lowercased, limited to `[a-z0-9-]`: "My-Laptop-2.local"
/// becomes "my-laptop-2".
pub fn default_device_id(hostname: &str) -> String {
    let label = hostname.split('.').next().unwrap_or("");
    let id: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let id = id.trim_matches('-');
    if id.is_empty() {
        "device".into()
    } else {
        id.into()
    }
}

fn pct(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
    }
}

/// Copies Linux readings into the snapshot and advances the swap-in mark.
/// Compiled on every OS so the Mac build type-checks and tests it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn apply_linux(
    s: &mut MetricsSnapshot,
    l: &linux::LinuxReadings,
    mark: &mut SwapInMark,
    now: Instant,
) {
    s.user_idle_secs = l.user_idle_secs;
    s.on_ac_power = l.power.on_ac;
    s.battery_pct = l.power.battery_pct;
    s.thermal_limited = l.thermal_limited;
    s.psi_mem_some_avg10 = l.psi_mem.some_avg10;
    s.psi_mem_full_avg10 = l.psi_mem.full_avg10;
    s.psi_io_full_avg10 = l.psi_io.full_avg10;
    s.swap_in_per_sec = rate_since(*mark, l.pswpin, now);
    *mark = l.pswpin.map(|c| (c, now));
}

/// Pages per second since the mark; None on the first reading, a counter reset or a zero
/// interval.
fn rate_since(mark: SwapInMark, current: Option<u64>, now: Instant) -> Option<f64> {
    mark.zip(current).and_then(|((prev, at), cur)| {
        let secs = now.saturating_duration_since(at).as_secs_f64();
        (cur >= prev && secs > 0.0).then(|| (cur - prev) as f64 / secs)
    })
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pressure_from_level(level: u32) -> Option<MemoryPressure> {
    match level {
        1 => Some(MemoryPressure::Normal),
        2 => Some(MemoryPressure::Warn),
        4 => Some(MemoryPressure::Critical),
        _ => None,
    }
}
