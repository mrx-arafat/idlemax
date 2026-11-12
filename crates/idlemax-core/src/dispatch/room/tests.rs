use super::*;
use crate::types::MemoryPressure;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

fn laptop() -> WorkerRoom {
    WorkerRoom::new(RoomPolicy::new(
        &DispatcherConfig::default(),
        &GuardPolicy::macos_laptop(),
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
    room.evaluate(&MetricsSnapshot {
        at_ms: m.at_ms - 90_000,
        ..busy_mac(0)
    });
    room.evaluate(&m)
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
    let first = room.evaluate(&busy_mac(1_000));
    assert_eq!(first.state, LineState::Hold);
    assert!(first.reason.contains("swap-in"), "{}", first.reason);
    assert_eq!(first.open_slots, 0);
    assert_eq!(room.evaluate(&busy_mac(61_000)).state, LineState::Run);
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
    assert_eq!(room.evaluate(&mid).state, LineState::Hold);
    assert_eq!(room.evaluate(&busy_mac(160_000)).state, LineState::Hold);
    assert_eq!(room.evaluate(&busy_mac(220_000)).state, LineState::Run);

    let high = |at| MetricsSnapshot {
        swap_in_per_sec: Some(500.0),
        ..busy_mac(at)
    };
    assert_eq!(room.evaluate(&high(300_000)).state, LineState::Hold);
    let shed = room.evaluate(&high(360_000));
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

    let mut guard = GuardPolicy::macos_laptop();
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
}
