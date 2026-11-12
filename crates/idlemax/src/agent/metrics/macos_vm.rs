//! Adapter: memory used as Activity Monitor shows it, and the swap-in counter, from macOS
//! `vm_stat`. The parser is pure and compiled everywhere so its tests run on any host; only
//! the reader is macOS-only.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::time::Instant;

use idlemax_core::types::MetricsSnapshot;

use super::{rate_since, SwapInMark};

/// The `vm_stat` counters the worker room needs, in pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmStat {
    pub page_size: u64,
    pub anonymous: u64,
    pub purgeable: u64,
    pub wired: u64,
    pub compressor: u64,
    pub swapins: u64,
}

impl VmStat {
    /// App, wired and compressed memory: (anonymous - purgeable + wired + compressor) pages.
    pub fn used_bytes(&self) -> u64 {
        self.anonymous
            .saturating_sub(self.purgeable)
            .saturating_add(self.wired)
            .saturating_add(self.compressor)
            .saturating_mul(self.page_size)
    }
}

/// Reads `vm_stat` output. A missing line makes the whole parse None, so the room fails
/// closed to `hold`.
pub fn parse_vm_stat(out: &str) -> Option<VmStat> {
    let page_size = out
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let count = |label: &str| -> Option<u64> {
        out.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == label)
                .then(|| value.trim().trim_end_matches('.').parse().ok())
                .flatten()
        })
    };
    Some(VmStat {
        page_size,
        anonymous: count("Anonymous pages")?,
        purgeable: count("Pages purgeable")?,
        wired: count("Pages wired down")?,
        compressor: count("Pages occupied by compressor")?,
        swapins: count("Swapins")?,
    })
}

/// Fills `mem_used_pct` and `swap_in_per_sec` from one `vm_stat` reading and advances the
/// swap-in mark.
pub fn apply(s: &mut MetricsSnapshot, vm: Option<&VmStat>, mark: &mut SwapInMark, now: Instant) {
    s.mem_used_pct = vm
        .filter(|_| s.mem_total_bytes > 0)
        .map(|v| (v.used_bytes() as f64 / s.mem_total_bytes as f64 * 100.0).clamp(0.0, 100.0));
    let swapins = vm.map(|v| v.swapins);
    s.swap_in_per_sec = rate_since(*mark, swapins, now);
    *mark = swapins.map(|c| (c, now));
}

#[cfg(target_os = "macos")]
pub fn read() -> Option<VmStat> {
    let out = super::cmd::run("/usr/bin/vm_stat", &[], super::cmd::PROBE_TIMEOUT)?;
    parse_vm_stat(&out)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SAMPLE: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                               12000.
Pages active:                            400000.
Anonymous pages:                         296432.
Pages purgeable:                          10000.
Pages wired down:                        200000.
Pages occupied by compressor:            300000.
Swapins:                                   1000.
Swapouts:                                  2000.
";

    #[test]
    fn macos_vm_used_memory_matches_activity_monitor_arithmetic() {
        let vm = parse_vm_stat(SAMPLE).unwrap();
        assert_eq!(vm.used_bytes(), 12_884_901_888);
        let mut s = MetricsSnapshot {
            mem_total_bytes: 17_179_869_184,
            ..Default::default()
        };
        apply(&mut s, Some(&vm), &mut None, Instant::now());
        assert_eq!(s.mem_used_pct, Some(75.0));
        let without = SAMPLE.replace("Swapins:                                   1000.\n", "");
        assert_eq!(parse_vm_stat(&without), None);
    }

    #[test]
    fn macos_vm_swap_in_rate_is_the_counter_rise_per_second() {
        let vm = |swapins| VmStat {
            swapins,
            ..parse_vm_stat(SAMPLE).unwrap()
        };
        let t0 = Instant::now();
        let mut mark = None;
        let mut s = MetricsSnapshot::default();
        apply(&mut s, Some(&vm(1000)), &mut mark, t0);
        assert_eq!(s.swap_in_per_sec, None);
        apply(
            &mut s,
            Some(&vm(1300)),
            &mut mark,
            t0 + Duration::from_secs(30),
        );
        assert_eq!(s.swap_in_per_sec, Some(10.0));
        apply(
            &mut s,
            Some(&vm(5)),
            &mut mark,
            t0 + Duration::from_secs(60),
        );
        assert_eq!(s.swap_in_per_sec, None);
    }
}
