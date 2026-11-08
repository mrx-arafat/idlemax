//! Tests: guard pause and kill thresholds for both presets, and the kill switch.

use super::*;
use crate::types::MemoryPressure;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const GB: u64 = 1_000_000_000;

fn mac() -> MetricsSnapshot {
    MetricsSnapshot {
        at_ms: 1_000_000,
        cpu_count: 10,
        load_avg_1m: 2.0,
        mem_total_bytes: 16 * GIB,
        mem_available_bytes: 8 * GIB,
        mem_free_pct: 50.0,
        swap_total_bytes: 6 * GIB,
        swap_used_bytes: 4 * GIB,
        disk_free_bytes: 100 * GIB,
        disk_total_bytes: 500 * GIB,
        user_idle_secs: Some(3600),
        on_ac_power: Some(true),
        battery_pct: Some(80.0),
        thermal_limited: Some(false),
        memory_pressure: Some(MemoryPressure::Normal),
        ..Default::default()
    }
}

fn linux() -> MetricsSnapshot {
    MetricsSnapshot {
        at_ms: 1_000_000,
        cpu_count: 2,
        load_avg_1m: 0.2,
        mem_total_bytes: 3379 * MIB,
        mem_available_bytes: 2 * GIB,
        swap_total_bytes: 2560 * MIB,
        swap_used_bytes: GIB,
        disk_free_bytes: 50 * GB,
        disk_total_bytes: 80 * GB,
        thermal_limited: Some(false),
        psi_mem_some_avg10: Some(0.0),
        psi_mem_full_avg10: Some(0.0),
        psi_io_full_avg10: Some(0.3),
        swap_in_per_sec: Some(0.0),
        ..Default::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum A {
    Run,
    Hold,
    Pause,
    Kill,
}

const MAC: bool = true;
const SERVER: bool = false;

/// (name, laptop preset, change to both samples, swap MiB added before the second
/// sample, kill switch, verdicts for two samples 30 s apart).
type Case = (
    &'static str,
    bool,
    fn(&mut MetricsSnapshot),
    u64,
    bool,
    (A, A),
);

#[test]
fn pauses_and_kills_at_preset_thresholds_and_on_the_kill_switch() {
    use A::{Hold, Kill, Pause, Run};
    let cases: &[Case] = &[
        ("mac healthy", MAC, |_| {}, 0, false, (Run, Run)),
        (
            "mac user active",
            MAC,
            |m| m.user_idle_secs = Some(599),
            0,
            false,
            (Pause, Pause),
        ),
        (
            "mac on battery",
            MAC,
            |m| m.on_ac_power = Some(false),
            0,
            false,
            (Pause, Pause),
        ),
        (
            "mac thermal",
            MAC,
            |m| m.thermal_limited = Some(true),
            0,
            false,
            (Pause, Pause),
        ),
        (
            "mac free 30%",
            MAC,
            |m| m.mem_free_pct = 30.0,
            0,
            false,
            (Run, Run),
        ),
        (
            "mac free 29.9%",
            MAC,
            |m| m.mem_free_pct = 29.9,
            0,
            false,
            (Pause, Pause),
        ),
        (
            "mac free 19.9%",
            MAC,
            |m| m.mem_free_pct = 19.9,
            0,
            false,
            (Pause, Kill),
        ),
        (
            "mac load 6.1",
            MAC,
            |m| m.load_avg_1m = 6.1,
            0,
            false,
            (Pause, Pause),
        ),
        (
            "mac load 8.1",
            MAC,
            |m| m.load_avg_1m = 8.1,
            0,
            false,
            (Pause, Kill),
        ),
        (
            "mac 4.6 GiB free of 228 GiB runs jobs",
            MAC,
            |m| {
                m.disk_total_bytes = 228 * GIB;
                m.disk_free_bytes = 4 * GIB + 600 * MIB;
            },
            0,
            false,
            (Run, Run),
        ),
        (
            "mac disk < 2 GiB holds new jobs only",
            MAC,
            |m| m.disk_free_bytes = 2 * GIB - MIB,
            0,
            false,
            (Hold, Hold),
        ),
        (
            "mac disk < 1 GiB",
            MAC,
            |m| m.disk_free_bytes = GIB - MIB,
            0,
            false,
            (Pause, Kill),
        ),
        (
            "mac swap +300 MiB in 30 s",
            MAC,
            |_| {},
            300,
            false,
            (Run, Pause),
        ),
        ("mac kill switch", MAC, |_| {}, 0, true, (Pause, Pause)),
        ("server healthy", SERVER, |_| {}, 0, false, (Run, Run)),
        (
            "server avail 1023 MiB",
            SERVER,
            |m| m.mem_available_bytes = 1023 * MIB,
            0,
            false,
            (Pause, Pause),
        ),
        (
            "server avail 767 MiB",
            SERVER,
            |m| m.mem_available_bytes = 767 * MIB,
            0,
            false,
            (Pause, Kill),
        ),
        (
            "server with 7.7 GiB of cold swap and no swap-in runs jobs",
            SERVER,
            |m| {
                m.cpu_count = 6;
                m.mem_total_bytes = 16 * GIB;
                m.mem_available_bytes = 10 * GIB;
                m.swap_total_bytes = 8 * GIB;
                m.swap_used_bytes = 7707 * MIB;
            },
            0,
            false,
            (Run, Run),
        ),
        (
            "server PSI mem some 5.1%",
            SERVER,
            |m| m.psi_mem_some_avg10 = Some(5.1),
            0,
            false,
            (Pause, Pause),
        ),
        (
            "server PSI mem full 1.1%",
            SERVER,
            |m| m.psi_mem_full_avg10 = Some(1.1),
            0,
            false,
            (Pause, Kill),
        ),
        (
            "server PSI io full 25.1%",
            SERVER,
            |m| m.psi_io_full_avg10 = Some(25.1),
            0,
            false,
            (Pause, Kill),
        ),
        (
            "server load 1.6",
            SERVER,
            |m| m.load_avg_1m = 1.6,
            0,
            false,
            (Pause, Pause),
        ),
        (
            "server load 2.1",
            SERVER,
            |m| m.load_avg_1m = 2.1,
            0,
            false,
            (Pause, Kill),
        ),
        (
            "server 2099 MiB free runs jobs",
            SERVER,
            |m| m.disk_free_bytes = 2099 * MIB,
            0,
            false,
            (Run, Run),
        ),
        (
            "server disk < 2 GiB holds new jobs only",
            SERVER,
            |m| m.disk_free_bytes = 2 * GIB - MIB,
            0,
            false,
            (Hold, Hold),
        ),
        (
            "server disk < 1 GiB",
            SERVER,
            |m| m.disk_free_bytes = GIB - MIB,
            0,
            false,
            (Pause, Kill),
        ),
    ];
    for &(name, laptop, change, swap_growth_mib, kill_switch, want) in cases {
        let policy = if laptop {
            GuardPolicy::macos_laptop()
        } else {
            GuardPolicy::linux_server()
        };
        let run = GuardAction::Run {
            slots: policy.max_jobs,
            mem_mib: policy.ram_budget_mib,
        };
        let action = |a: A| match a {
            A::Run => run,
            A::Hold => GuardAction::Hold,
            A::Pause => GuardAction::Pause,
            A::Kill => GuardAction::Kill,
        };
        let mut guard = Guard::new(policy);
        let mut m = if laptop { mac() } else { linux() };
        change(&mut m);
        let first = guard.evaluate(&m, &[], kill_switch, false).action;
        m.at_ms += 30_000;
        m.swap_used_bytes += swap_growth_mib * MIB;
        let second = guard.evaluate(&m, &[], kill_switch, false).action;
        assert_eq!((first, second), (action(want.0), action(want.1)), "{name}");
    }
}

#[test]
fn a_server_pauses_for_sustained_swap_in_however_little_swap_is_used() {
    let mut guard = Guard::new(GuardPolicy::linux_server());
    let mut m = linux();
    m.swap_used_bytes = 100 * MIB;
    m.swap_in_per_sec = Some(500.0);
    let mut verdicts = Vec::new();
    for _ in 0..3 {
        verdicts.push(guard.evaluate(&m, &[], false, false).action);
        m.at_ms += 30_000;
    }
    assert_eq!(
        (verdicts[0].may_start(), verdicts[1].may_start()),
        (true, true),
        "swap-in must stay high for swap_in_sustain_secs first"
    );
    assert_eq!(verdicts[2], GuardAction::Pause);
}

#[test]
fn pins_kill_switch_and_local_pause_reasons_distinctly() {
    let policy = GuardPolicy::macos_laptop();
    let mut guard = Guard::new(policy.clone());
    let m = mac();

    let d_none = guard.evaluate(&m, &[], false, false);
    assert_eq!(
        d_none.action,
        GuardAction::Run {
            slots: policy.max_jobs,
            mem_mib: policy.ram_budget_mib,
        }
    );
    assert!(!d_none.reasons.iter().any(|r| r.contains("pause")));

    let mut guard = Guard::new(policy.clone());
    let d_kill = guard.evaluate(&m, &[], true, false);
    assert_eq!(d_kill.action, GuardAction::Pause);
    assert!(d_kill
        .reasons
        .contains(&"pause: kill switch file PAUSE present".to_string()));
    assert!(!d_kill
        .reasons
        .contains(&"pause: local pause active".to_string()));

    let mut guard = Guard::new(policy.clone());
    let d_local = guard.evaluate(&m, &[], false, true);
    assert_eq!(d_local.action, GuardAction::Pause);
    assert!(d_local
        .reasons
        .contains(&"pause: local pause active".to_string()));
    assert!(!d_local
        .reasons
        .contains(&"pause: kill switch file PAUSE present".to_string()));

    let mut guard = Guard::new(policy);
    let d_both = guard.evaluate(&m, &[], true, true);
    assert_eq!(d_both.action, GuardAction::Pause);
    assert!(d_both
        .reasons
        .contains(&"pause: kill switch file PAUSE present".to_string()));
    assert!(d_both
        .reasons
        .contains(&"pause: local pause active".to_string()));
}

#[test]
fn unknown_idle_time_holds_only_when_an_idle_minimum_is_required() {
    let mut m = mac();
    m.user_idle_secs = None;

    let mut guard = Guard::new(GuardPolicy::macos_laptop());
    let held = guard.evaluate(&m, &[], false, false);
    assert_eq!(held.action, GuardAction::Hold);
    assert!(held
        .reasons
        .contains(&"hold: user idle time unknown".to_string()));

    // 0 means "start whatever the user is doing", so there is nothing to wait for.
    let policy = GuardPolicy {
        user_idle_min_secs: Some(0),
        ..GuardPolicy::macos_laptop()
    };
    let run = GuardAction::Run {
        slots: policy.max_jobs,
        mem_mib: policy.ram_budget_mib,
    };
    let free = Guard::new(policy).evaluate(&m, &[], false, false);
    assert_eq!(free.action, run);
    assert!(free.reasons.is_empty(), "{:?}", free.reasons);
}
