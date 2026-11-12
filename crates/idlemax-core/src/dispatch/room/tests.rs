use super::*;
use crate::dispatch::linux::{linux_numbers, LinuxFacts, LinuxNumbers};
use crate::types::{DeviceProfile, MemoryPressure};

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

fn laptop() -> WorkerRoom {
    WorkerRoom::new(RoomPolicy::new(
        &DispatcherConfig::default(),
        &GuardPolicy::laptop(),
    ))
}

/// 16 GiB at 80 percent used, 9 GiB free disk, swap-in 2 pages/s, the owner typing, memory
/// pressure `Warn` and on battery at 80 percent.
fn busy_mac(at_ms: UnixMs) -> MetricsSnapshot {
    MetricsSnapshot {
        at_ms,
        mem_total_bytes: 16 * GIB,
        mem_used_pct: Some(80.0),
        disk_total_bytes: 500 * GIB,
        disk_free_bytes: 9 * GIB,
        swap_in_per_sec: Some(2.0),
        user_idle_secs: Some(0),
        memory_pressure: Some(MemoryPressure::Warn),
        on_ac_power: Some(false),
        battery_pct: Some(80.0),
        ..Default::default()
    }
}

/// The room after a minute of low swap-in, so only the reading under test sets the state.
fn settled(room: &mut WorkerRoom, m: MetricsSnapshot) -> RoomDecision {
    room.evaluate(
        &MetricsSnapshot {
            at_ms: m.at_ms - 90_000,
            ..busy_mac(0)
        },
        None,
    );
    room.evaluate(&m, None)
}

#[test]
fn a_busy_owner_under_warn_pressure_still_leaves_three_slots() {
    let mut room = laptop();
    let d = settled(&mut room, busy_mac(100_000));
    assert_eq!(d.state, LineState::Run, "{}", d.reason);
    assert_eq!(d.open_slots, 3);
    assert!(d.reason.contains("memory 80% used"), "{}", d.reason);
}

#[test]
fn swap_in_must_be_low_for_a_minute_before_workers_start() {
    let mut room = laptop();
    let first = room.evaluate(&busy_mac(1_000), None);
    assert_eq!(first.state, LineState::Hold);
    assert!(first.reason.contains("swap-in"), "{}", first.reason);
    assert_eq!(first.open_slots, 0);
    assert_eq!(room.evaluate(&busy_mac(61_000), None).state, LineState::Run);
}

#[test]
fn memory_and_disk_lines_set_the_state() {
    for (pct, state) in [
        (89.9, LineState::Run),
        (90.0, LineState::Hold),
        (95.0, LineState::Shed),
        (98.0, LineState::Emergency),
    ] {
        let m = MetricsSnapshot {
            mem_used_pct: Some(pct),
            ..busy_mac(100_000)
        };
        let d = settled(&mut laptop(), m);
        assert_eq!(d.state, state, "{pct}: {}", d.reason);
        if state != LineState::Run {
            assert_eq!(d.open_slots, 0);
        }
    }
    for (free_mib, state) in [
        (4095, LineState::Hold),
        (2047, LineState::Shed),
        (1023, LineState::Emergency),
    ] {
        let m = MetricsSnapshot {
            disk_free_bytes: free_mib * MIB,
            ..busy_mac(100_000)
        };
        let d = settled(&mut laptop(), m);
        assert_eq!(d.state, state, "{free_mib}: {}", d.reason);
        assert_eq!(d.open_slots, 0);
    }
    let held = settled(
        &mut laptop(),
        MetricsSnapshot {
            mem_used_pct: Some(91.0),
            ..busy_mac(100_000)
        },
    );
    assert_eq!(held.reason, "hold: memory 91% used (hold at 90%)");
}

#[test]
fn swap_in_above_low_holds_for_a_minute_and_high_for_a_minute_sheds() {
    let mut room = laptop();
    assert_eq!(settled(&mut room, busy_mac(100_000)).state, LineState::Run);
    let mid = MetricsSnapshot {
        swap_in_per_sec: Some(50.0),
        ..busy_mac(130_000)
    };
    assert_eq!(room.evaluate(&mid, None).state, LineState::Hold);
    assert_eq!(
        room.evaluate(&busy_mac(160_000), None).state,
        LineState::Hold
    );
    assert_eq!(
        room.evaluate(&busy_mac(220_000), None).state,
        LineState::Run
    );

    let high = |at| MetricsSnapshot {
        swap_in_per_sec: Some(500.0),
        ..busy_mac(at)
    };
    assert_eq!(room.evaluate(&high(300_000), None).state, LineState::Hold);
    let shed = room.evaluate(&high(360_000), None);
    assert_eq!(shed.state, LineState::Shed, "{}", shed.reason);
}

#[test]
fn a_low_battery_and_heat_hold_unless_the_policy_ignores_them() {
    let low = MetricsSnapshot {
        battery_pct: Some(39.0),
        ..busy_mac(100_000)
    };
    let d = settled(&mut laptop(), low.clone());
    assert_eq!(d.state, LineState::Hold);
    assert!(d.reason.contains("battery 39% (floor 40%)"), "{}", d.reason);
    let hot = MetricsSnapshot {
        thermal_limited: Some(true),
        ..busy_mac(100_000)
    };
    let d = settled(&mut laptop(), hot.clone());
    assert_eq!(d.state, LineState::Hold);
    assert!(d.reason.contains("thermal limited"), "{}", d.reason);

    let mut guard = GuardPolicy::laptop();
    guard.min_battery_pct = None;
    guard.pause_on_thermal = false;
    let lax = || WorkerRoom::new(RoomPolicy::new(&DispatcherConfig::default(), &guard));
    assert_eq!(settled(&mut lax(), low).state, LineState::Run);
    assert_eq!(settled(&mut lax(), hot).state, LineState::Run);
}

#[test]
fn no_memory_reading_holds_with_no_slots() {
    let m = MetricsSnapshot {
        mem_used_pct: None,
        ..busy_mac(100_000)
    };
    let d = settled(&mut laptop(), m);
    assert_eq!((d.state, d.open_slots), (LineState::Hold, 0));
    assert!(d.reason.contains("not read"), "{}", d.reason);
    let unread = laptop().evaluate(
        &MetricsSnapshot {
            swap_in_per_sec: None,
            ..busy_mac(100_000)
        },
        None,
    );
    assert!(
        unread
            .reason
            .ends_with("hold: swap-in is not read on this system"),
        "{}",
        unread.reason
    );
}

fn linux_numbers_of(profile: DeviceProfile, total_mib: u64) -> LinuxNumbers {
    let guard =
        GuardPolicy::for_profile(profile).server_sized_for(8, total_mib * MIB, &toml::Table::new());
    linux_numbers(profile, &guard, &DispatcherConfig::default())
}

/// A Linux sample: no memory used percent, calm pressure, 9 GiB free disk, swap-in 500.
fn linux_sample(total_mib: u64, available_mib: u64) -> MetricsSnapshot {
    MetricsSnapshot {
        at_ms: 100_000,
        mem_total_bytes: total_mib * MIB,
        mem_available_bytes: available_mib * MIB,
        disk_total_bytes: 500 * GIB,
        disk_free_bytes: 9 * GIB,
        swap_in_per_sec: Some(500.0),
        psi_mem_some_avg10: Some(0.0),
        psi_mem_full_avg10: Some(0.0),
        ..Default::default()
    }
}

fn on_linux(m: &MetricsSnapshot, facts: &LinuxFacts, numbers: &LinuxNumbers) -> RoomDecision {
    laptop().evaluate(m, Some((facts, numbers)))
}

fn facts_of(m: &MetricsSnapshot, outside_mib: u64, tracked: u32) -> LinuxFacts {
    LinuxFacts {
        mem_total_bytes: m.mem_total_bytes,
        mem_available_bytes: m.mem_available_bytes,
        outside_used_bytes: outside_mib * MIB,
        tracked,
        ..Default::default()
    }
}

#[test]
fn a_production_share_sets_the_slots_and_nothing_tracked_is_never_an_emergency() {
    let n = linux_numbers_of(DeviceProfile::Production, 16384);
    let m = linux_sample(16384, 8192);
    let d = on_linux(&m, &facts_of(&m, 3072, 0), &n);
    assert_eq!((d.state, d.open_slots), (LineState::Run, 1), "{}", d.reason);
    assert!(d.reason.contains("75% of 4096 MiB"), "{}", d.reason);

    let n = linux_numbers_of(DeviceProfile::Production, 3379);
    let m = linux_sample(3379, 1200);
    let idle = on_linux(&m, &facts_of(&m, 0, 0), &n);
    assert_eq!(idle.state, LineState::Hold, "{}", idle.reason);
    assert!(
        idle.reason.contains("nothing of idlemax's is running"),
        "{}",
        idle.reason
    );
    let busy = on_linux(&m, &facts_of(&m, 0, 1), &n);
    assert_eq!(busy.state, LineState::Emergency, "{}", busy.reason);
}

#[test]
fn workbox_pressure_holds_and_sheds_and_swap_in_is_ignored() {
    let n = linux_numbers_of(DeviceProfile::Workbox, 16384);
    let at = |some, full| {
        let m = MetricsSnapshot {
            psi_mem_some_avg10: some,
            psi_mem_full_avg10: full,
            ..linux_sample(16384, 8192)
        };
        on_linux(&m, &facts_of(&m, 0, 1), &n)
    };
    assert_eq!(at(Some(9.9), Some(4.9)).state, LineState::Run);
    assert_eq!(at(Some(10.0), Some(0.0)).state, LineState::Hold);
    assert_eq!(at(Some(0.0), Some(5.0)).state, LineState::Shed);
    let unread = at(None, None);
    assert_eq!(unread.state, LineState::Hold);
    assert!(unread.reason.contains("psi=1"), "{}", unread.reason);
}

#[test]
fn a_linux_reading_that_fails_only_holds() {
    let n = linux_numbers_of(DeviceProfile::Workbox, 16384);
    let m = linux_sample(16384, 0);
    let facts = LinuxFacts {
        cgroup_limit_bytes: Some(GIB),
        ..facts_of(&m, 0, 3)
    };
    let d = on_linux(&m, &facts, &n);
    assert_eq!(
        (d.state, d.open_slots),
        (LineState::Hold, 0),
        "{}",
        d.reason
    );
    assert!(d.reason.contains("cgroup"), "{}", d.reason);
}
