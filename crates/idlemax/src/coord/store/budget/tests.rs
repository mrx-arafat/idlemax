use idlemax_core::types::{QuotaSnapshot, QuotaWindowKind};
use idlemax_core::worker::run::{Run, RunLaunch, RunState};

use super::super::quota::insert_snapshots;
use super::*;

const T0: UnixMs = 1_800_000_000_000;

fn weekly(used: f64, at: UnixMs) -> QuotaSnapshot {
    QuotaSnapshot {
        account: "max".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: used,
        resets_at_ms: Some(T0 + 86_400_000),
        source: Default::default(),
        at_ms: at,
    }
}

#[test]
fn a_sample_takes_its_after_reading_from_the_first_snapshot_after_its_end() {
    let mut s = Store::open_in_memory().unwrap();
    let stored = s
        .insert_run(
            &Run {
                device: "mac".into(),
                project: "app".into(),
                task: "roadmap:1:x".into(),
                source: "roadmap".into(),
                account: "max".into(),
                model: "sonnet".into(),
                start_ms: T0,
                updated_ms: T0,
                ..Run::default()
            },
            &RunLaunch::default(),
        )
        .unwrap();
    let ended = Run {
        state: RunState::Running,
        launches: 1,
        end_ms: Some(T0 + 600_000),
        quota_before: vec![weekly(40.0, T0 - 60_000)],
        quota_after: vec![weekly(41.0, T0 + 300_000)],
        ..stored
    };
    assert!(s.update_run(&ended, RunState::Starting).unwrap());

    let samples = s.run_samples_since(T0 - 1).unwrap();
    assert!(samples[0].after.is_empty(), "{:?}", samples[0].after);

    insert_snapshots(&s.conn, &[weekly(43.0, T0 + 900_000)], T0 + 900_000).unwrap();
    let samples = s.run_samples_since(T0 - 1).unwrap();
    assert_eq!(samples[0].after, [weekly(43.0, T0 + 900_000)]);
    assert_eq!(samples[0].task_type, "roadmap");
}
