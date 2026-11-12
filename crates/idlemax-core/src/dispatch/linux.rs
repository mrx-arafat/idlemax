//! Domain: the worker room's Linux inputs (D19): the numbers of each profile, the percent of
//! idlemax's memory budget in use, and the memory pressure lines. Pure.
//!
//! The percent is what idlemax may fill, not the whole machine: the reserve stays with the
//! system or its services, and on a `production` host the share caps the rest.

use crate::config::{DispatcherConfig, GuardPolicy};
use crate::protocol::RunningJob;
use crate::types::{DeviceProfile, MetricsSnapshot};

use super::room::{num, LineState};

const MIB: u64 = 1024 * 1024;

/// Memory pressure `some avg10` that holds new workers, per profile (A25).
pub const PSI_SOME_HOLD_PCT: f64 = 5.0;
pub const WORKBOX_PSI_SOME_HOLD_PCT: f64 = 10.0;
/// Memory pressure `full avg10` that sheds a worker, per profile (A25).
pub const PSI_FULL_SHED_PCT: f64 = 1.0;
pub const WORKBOX_PSI_FULL_SHED_PCT: f64 = 5.0;

/// The numbers of one Linux device.
#[derive(Debug, Clone, PartialEq)]
pub struct LinuxNumbers {
    /// Memory kept for the system or the services.
    pub reserve_mib: u64,
    /// The most idlemax may fill; `None` is all but the reserve.
    pub share_mib: Option<u64>,
    pub psi_some_hold_pct: f64,
    pub psi_full_shed_pct: f64,
}

/// What one sample and the agent's own tracking say about memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinuxFacts {
    pub mem_total_bytes: u64,
    pub mem_available_bytes: u64,
    /// Set only when new workers would run inside the agent's cgroup.
    pub cgroup_limit_bytes: Option<u64>,
    /// What that cgroup uses, when it has a limit.
    pub cgroup_used_bytes: Option<u64>,
    /// Tracked jobs and workers that run outside that cgroup.
    pub outside_used_bytes: u64,
    /// Dispatcher jobs and workers this agent tracks.
    pub tracked: u32,
}

/// The percent of idlemax's budget in use, and the size of that budget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinuxRoom {
    pub used_pct: f64,
    pub budget_mib: u64,
}

/// The profile's numbers, with the `dispatcher.linux_*` keys that are set in their place.
/// `guard` is the sized policy, whose `ram_budget_mib` is a `production` host's share.
pub fn linux_numbers(
    profile: DeviceProfile,
    guard: &GuardPolicy,
    cfg: &DispatcherConfig,
) -> LinuxNumbers {
    let (reserve_mib, share_mib, some, full) = match profile {
        DeviceProfile::Laptop => (0, None, PSI_SOME_HOLD_PCT, PSI_FULL_SHED_PCT),
        DeviceProfile::Workbox => (
            crate::config::WORKBOX_RAM_RESERVE_MIB,
            None,
            WORKBOX_PSI_SOME_HOLD_PCT,
            WORKBOX_PSI_FULL_SHED_PCT,
        ),
        DeviceProfile::Production => (
            crate::config::SERVER_RAM_RESERVE_MIB,
            Some(guard.ram_budget_mib),
            PSI_SOME_HOLD_PCT,
            PSI_FULL_SHED_PCT,
        ),
    };
    LinuxNumbers {
        reserve_mib: cfg.linux_reserve_mib.unwrap_or(reserve_mib),
        share_mib,
        psi_some_hold_pct: cfg.linux_psi_some_hold_pct.unwrap_or(some),
        psi_full_shed_pct: cfg.linux_psi_full_shed_pct.unwrap_or(full),
    }
}

/// The facts of one sample. `cgroup` is the agent's cgroup limit and use, passed only while
/// new workers would run inside it; then a job counts as outside only when `scoped_jobs`
/// says each job runs in its own systemd scope. A job with no reading counts its `mem_mb`.
pub fn linux_facts(
    m: &MetricsSnapshot,
    cgroup: Option<(u64, Option<u64>)>,
    scoped_jobs: bool,
    running: &[RunningJob],
    tracked: u32,
) -> LinuxFacts {
    let outside = if cgroup.is_none() || scoped_jobs {
        running
            .iter()
            .map(|j| j.rss_bytes.unwrap_or(j.mem_mb.saturating_mul(MIB)))
            .fold(0u64, u64::saturating_add)
    } else {
        0
    };
    LinuxFacts {
        mem_total_bytes: m.mem_total_bytes,
        mem_available_bytes: m.mem_available_bytes,
        cgroup_limit_bytes: cgroup.map(|(limit, _)| limit),
        cgroup_used_bytes: cgroup.and_then(|(_, used)| used),
        outside_used_bytes: outside,
        tracked,
    }
}

/// The percent of what idlemax may fill that is in use; an `Err` names the missing reading.
pub fn linux_room(facts: &LinuxFacts, numbers: &LinuxNumbers) -> Result<LinuxRoom, String> {
    if facts.mem_total_bytes == 0 {
        return Err("memory total is not read on this system".into());
    }
    let cgroup_used = match (facts.cgroup_limit_bytes, facts.cgroup_used_bytes) {
        (Some(_), None) => {
            return Err("the memory use of the agent's cgroup is not read".into());
        }
        (_, used) => used.unwrap_or(0),
    };
    let share = match (
        numbers.share_mib.map(|s| s.saturating_mul(MIB)),
        facts.cgroup_limit_bytes,
    ) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let reserve = numbers.reserve_mib.saturating_mul(MIB);
    let mut budget = facts.mem_total_bytes.saturating_sub(reserve);
    let mut room = facts.mem_available_bytes.saturating_sub(reserve);
    if let Some(share) = share {
        let own = cgroup_used.saturating_add(facts.outside_used_bytes);
        budget = budget.min(share);
        room = room.min(share.saturating_sub(own));
    }
    if budget < MIB {
        return Err(format!(
            "no memory is left for idlemax after the reserve of {} MiB",
            numbers.reserve_mib
        ));
    }
    let used = budget - room.min(budget);
    Ok(LinuxRoom {
        used_pct: used as f64 * 100.0 / budget as f64,
        budget_mib: budget / MIB,
    })
}

/// The pressure lines: `some avg10` holds and `full avg10` sheds; never an emergency (D14).
pub fn pressure_lines(m: &MetricsSnapshot, numbers: &LinuxNumbers) -> Vec<(LineState, String)> {
    let (Some(some), Some(full)) = (m.psi_mem_some_avg10, m.psi_mem_full_avg10) else {
        return vec![(
            LineState::Hold,
            "memory pressure is not read on this system: boot the kernel with psi=1".into(),
        )];
    };
    let mut found = Vec::new();
    if full >= numbers.psi_full_shed_pct {
        found.push((
            LineState::Shed,
            format!(
                "memory pressure full {}% (shed at {}%)",
                num(full),
                num(numbers.psi_full_shed_pct)
            ),
        ));
    }
    if some >= numbers.psi_some_hold_pct {
        found.push((
            LineState::Hold,
            format!(
                "memory pressure some {}% (hold at {}%)",
                num(some),
                num(numbers.psi_some_hold_pct)
            ),
        ));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn facts(total_mib: u64, available_mib: u64) -> LinuxFacts {
        LinuxFacts {
            mem_total_bytes: total_mib * MIB,
            mem_available_bytes: available_mib * MIB,
            ..Default::default()
        }
    }

    fn numbers(profile: DeviceProfile, total_mib: u64) -> LinuxNumbers {
        let guard = GuardPolicy::for_profile(profile).server_sized_for(
            8,
            total_mib * MIB,
            &toml::Table::new(),
        );
        linux_numbers(profile, &guard, &DispatcherConfig::default())
    }

    fn used(f: &LinuxFacts, n: &LinuxNumbers) -> f64 {
        linux_room(f, n).unwrap().used_pct
    }

    fn near(got: f64, want: f64) {
        assert!((got - want).abs() < 1e-6, "{got} != {want}");
    }

    #[test]
    fn a_workbox_reads_its_available_memory_over_all_but_the_reserve() {
        let n = numbers(DeviceProfile::Workbox, 16384);
        near(used(&facts(16384, 4096), &n), 80.0);
        near(used(&facts(16384, 2560), &n), 90.0);
        near(used(&facts(16384, 1024), &n), 100.0);
        near(used(&facts(16384, 512), &n), 100.0);
    }

    #[test]
    fn a_production_host_reads_its_share() {
        let n = numbers(DeviceProfile::Production, 16384);
        assert_eq!(n.share_mib, Some(4096));
        let f = LinuxFacts {
            outside_used_bytes: 3072 * MIB,
            ..facts(16384, 8192)
        };
        let room = linux_room(&f, &n).unwrap();
        near(room.used_pct, 75.0);
        assert_eq!(room.budget_mib, 4096);

        let small = numbers(DeviceProfile::Production, 3379);
        near(used(&facts(3379, 1200), &small), 100.0);
    }

    #[test]
    fn a_cgroup_limit_caps_the_room_and_counts_use_outside_it() {
        let n = numbers(DeviceProfile::Workbox, 16384);
        let capped = LinuxFacts {
            cgroup_limit_bytes: Some(2 * GIB),
            cgroup_used_bytes: Some(GIB),
            ..facts(16384, 12288)
        };
        near(used(&capped, &n), 50.0);
        let outside = LinuxFacts {
            outside_used_bytes: 512 * MIB,
            ..capped.clone()
        };
        near(used(&outside, &n), 75.0);
        let unknown = LinuxFacts {
            cgroup_used_bytes: None,
            ..capped
        };
        let err = linux_room(&unknown, &n).unwrap_err();
        assert!(err.contains("cgroup"), "{err}");
    }

    #[test]
    fn a_laptop_keeps_no_reserve_and_a_missing_total_is_an_error() {
        let n = numbers(DeviceProfile::Laptop, 16384);
        let f = LinuxFacts {
            mem_available_bytes: 16384 * MIB / 10,
            ..facts(16384, 0)
        };
        near(used(&f, &n), 90.0);
        assert!(linux_room(&facts(0, 0), &n).is_err());
    }

    #[test]
    fn the_facts_count_a_job_without_a_reading_at_its_budget() {
        let job = |rss| RunningJob {
            mem_mb: 512,
            rss_bytes: rss,
            ..Default::default()
        };
        let running = [job(Some(100 * MIB)), job(None)];
        let m = MetricsSnapshot::default();
        assert_eq!(
            linux_facts(&m, None, false, &running, 1).outside_used_bytes,
            612 * MIB
        );
        let inside = linux_facts(&m, Some((GIB, Some(MIB))), false, &running, 1);
        assert_eq!(
            (inside.cgroup_used_bytes, inside.outside_used_bytes),
            (Some(MIB), 0)
        );
    }
}
