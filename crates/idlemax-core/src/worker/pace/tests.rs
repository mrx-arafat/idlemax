//! Tests: the launch gate, the review cap, which task is open and the week's spend.

use super::*;
use crate::quota::Provider;
use crate::types::QuotaWindowKind;
use crate::worker::report::{CheckFacts, ReviewFacts};
use crate::worker::run::{RunFacts, Verdict};

fn run(id: i64, project: &str, task: &str, state: RunState, start: UnixMs) -> Run {
    Run {
        id,
        device: "mac".into(),
        project: project.into(),
        task: task.into(),
        account: "max".into(),
        state,
        start_ms: start,
        launches: 1,
        ..Default::default()
    }
}

fn usable(ok: bool) -> AccountUse {
    AccountUse {
        account: "max".into(),
        provider: Provider::Claude,
        usable: ok,
        why: if ok {
            "under the cap"
        } else {
            "5-hour window 61% used"
        }
        .into(),
        ..Default::default()
    }
}

fn room(state: LineState, slots: u32) -> RoomDecision {
    RoomDecision {
        state,
        reason: "memory 70%".into(),
        open_slots: slots,
        at_ms: 1,
    }
}

#[test]
fn may_launch_needs_every_condition() {
    let open = room(LineState::Run, 1);
    let account = usable(true);
    let gate = |runs: &'static [Run]| LaunchGate {
        now_ms: 100_000,
        held: None,
        account_paused: false,
        room: &open,
        device_runs: runs,
        last_launch_ms: Some(30_000),
        settle_ms: 60_000,
        reported: &[7],
        cli_allowed: true,
        account: Some(&account),
    };
    assert_eq!(may_launch(&gate(&[]), None), None);
    let checks: Vec<(LaunchGate<'_>, &str)> = vec![
        (
            LaunchGate {
                held: Some("offline"),
                ..gate(&[])
            },
            "offline",
        ),
        (
            LaunchGate {
                account_paused: true,
                ..gate(&[])
            },
            "paused",
        ),
        (
            LaunchGate {
                last_launch_ms: Some(50_000),
                ..gate(&[])
            },
            "settling",
        ),
        (
            LaunchGate {
                cli_allowed: false,
                ..gate(&[])
            },
            "workers.clis",
        ),
        (
            LaunchGate {
                account: None,
                ..gate(&[])
            },
            "not configured",
        ),
    ];
    for (g, want) in checks {
        let why = may_launch(&g, None).unwrap();
        assert!(why.contains(want), "{why}");
    }
    let hold = room(LineState::Hold, 0);
    assert!(may_launch(
        &LaunchGate {
            room: &hold,
            ..gate(&[])
        },
        None
    )
    .is_some());
    let full = room(LineState::Run, 0);
    assert!(may_launch(
        &LaunchGate {
            room: &full,
            ..gate(&[])
        },
        None
    )
    .is_some());
    let spent = usable(false);
    let why = may_launch(
        &LaunchGate {
            account: Some(&spent),
            ..gate(&[])
        },
        None,
    )
    .unwrap();
    assert!(why.contains("5-hour"), "{why}");
    assert!(may_launch(&gate(&[]), Some(7))
        .unwrap()
        .contains("still reports"));
    static STARTING: [Run; 1] = [Run {
        id: 1,
        device: String::new(),
        project: String::new(),
        task: String::new(),
        task_title: String::new(),
        source: String::new(),
        branch: None,
        worktree: None,
        provider: Provider::Claude,
        account: String::new(),
        model: String::new(),
        session: None,
        state: RunState::Starting,
        verdict: None,
        concern: None,
        reason: String::new(),
        summary: None,
        facts: RunFacts {
            check: None,
            end_text: None,
        },
        job: None,
        launches: 1,
        start_tries: 0,
        start_ms: 0,
        running_ms: None,
        end_ms: None,
        updated_ms: 0,
        quota_before: Vec::new(),
        quota_after: Vec::new(),
    }];
    assert!(may_launch(&gate(&STARTING), None)
        .unwrap()
        .contains("still starting"));
}

#[test]
fn the_cap_counts_waiting_and_in_flight_work_but_not_rejected() {
    let runs = [
        run(1, "a", "t1", RunState::Clean, 1),
        run(2, "a", "t2", RunState::NeedsYou, 2),
        run(3, "a", "t3", RunState::Running, 3),
        run(4, "a", "t4", RunState::Rejected, 4),
        run(5, "b", "t5", RunState::Clean, 5),
    ];
    assert_eq!(waiting(&runs, "a"), 2);
    assert!(under_cap(&runs, "a", 3).is_some());
    assert_eq!(under_cap(&runs, "b", 3), None);
}

#[test]
fn a_task_is_open_until_a_run_holds_it_and_closed_after_two_failures() {
    let clean = [run(1, "a", "t", RunState::Clean, 1)];
    assert_eq!(task_open(&clean, "t"), TaskOpen::Busy);
    let discarded = [run(1, "a", "t", RunState::Discarded, 1)];
    assert_eq!(task_open(&discarded, "t"), TaskOpen::Open);
    let rejected = [
        run(1, "a", "t", RunState::Rejected, 1),
        run(2, "a", "t", RunState::Rejected, 2),
    ];
    assert_eq!(task_open(&rejected, "t"), TaskOpen::Closed);
    assert_eq!(task_open(&[], "t"), TaskOpen::Open);

    // One failed check leaves the task open for a second attempt (Spec 3 Task 10, D10.6).
    let once = [run(1, "a", "t", RunState::Rejected, 1)];
    assert_eq!(task_open(&once, "t"), TaskOpen::Open);
    let mut worked = run(1, "a", "t", RunState::Discarded, 1);
    worked.verdict = Some(Verdict::NeedsYou);
    let then_discarded = [worked, run(2, "a", "t", RunState::Rejected, 2)];
    assert_eq!(task_open(&then_discarded, "t"), TaskOpen::Closed);
}

#[test]
fn a_run_discarded_before_it_did_anything_is_no_try_and_no_spend() {
    let mut never = [
        run(1, "a", "t", RunState::Discarded, 1_000),
        run(2, "a", "t", RunState::Discarded, 2_000),
    ];
    never[0].end_ms = Some(1_500);
    never[1].end_ms = Some(2_500);
    assert_eq!(task_open(&never, "t"), TaskOpen::Open);
    let readings = [weekly(40.0, 1_000), weekly(45.0, 2_500)];
    assert_eq!(week_spend(&never, "max", &readings, 0, 10_000), 0.0);

    // One that ran, even with no facts yet (discarded mid-work), is a try and spends.
    let mut ran = never.clone();
    ran[0].running_ms = Some(1_100);
    ran[1].running_ms = Some(2_100);
    assert_eq!(task_open(&ran, "t"), TaskOpen::Closed);
    assert!(week_spend(&ran, "max", &readings, 0, 10_000) > 0.0);
}

fn weekly(pct: f64, at: UnixMs) -> QuotaSnapshot {
    QuotaSnapshot {
        account: "max".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: pct,
        resets_at_ms: None,
        source: Default::default(),
        at_ms: at,
    }
}

#[test]
fn week_spend_counts_overlapping_runs_once() {
    let mut a = run(1, "a", "t1", RunState::Clean, 1_000);
    a.end_ms = Some(3_000);
    let mut b = run(2, "a", "t2", RunState::Clean, 2_000);
    b.end_ms = Some(4_000);
    let readings = [
        weekly(40.0, 1_000),
        weekly(41.0, 2_000),
        weekly(43.0, 3_000),
        weekly(44.0, 4_000),
    ];
    let spent = week_spend(&[a.clone(), b], "max", &readings, 0, 10_000);
    assert!((spent - 4.0).abs() < 1e-9, "{spent}");
    // A review the account paid for in a gap counts too.
    let mut reviewed = run(3, "z", "t3", RunState::Clean, 9_000);
    reviewed.account = "other".into();
    reviewed.facts = RunFacts {
        check: Some(CheckFacts {
            review: Some(ReviewFacts {
                account: "max".into(),
                quota_before: vec![weekly(44.0, 5_000)],
                quota_after: vec![weekly(46.0, 6_000)],
                ..Default::default()
            }),
            ..Default::default()
        }),
        end_text: None,
    };
    let more = [weekly(44.0, 5_000), weekly(46.0, 6_000)];
    let all: Vec<QuotaSnapshot> = readings.iter().chain(&more).cloned().collect();
    let spent = week_spend(&[a, reviewed], "max", &all, 0, 10_000);
    assert!((spent - 5.0).abs() < 1e-9, "{spent}");
    assert_eq!(measuring_week_end(1_000), 1_000 + MEASURING_WEEK_MS);
}
