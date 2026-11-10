//! Tests: one per state a device can be in, and the account roll-up.

use super::*;
use crate::protocol::RunningJob;
use crate::quota::{QuotaView, WindowState};
use crate::types::{DeviceInfo, GuardAction, GuardDecision, JobSpec, JobStatus, QuotaWindowKind};

const NOW: UnixMs = 1_000_000;

fn run(slots: u32, mem_mib: u64) -> GuardDecision {
    let action = GuardAction::Run { slots, mem_mib };
    GuardDecision {
        action,
        resource_action: action,
        ..Default::default()
    }
}

fn device(guard: GuardDecision) -> DeviceView {
    DeviceView {
        info: DeviceInfo {
            id: "mac".into(),
            capabilities: vec!["git".into()],
            ..Default::default()
        },
        online: true,
        last_seen_ms: NOW,
        guard: Some(guard),
        ..Default::default()
    }
}

fn running(id: i64, mem_mb: u64) -> RunningJob {
    RunningJob {
        id,
        status: JobStatus::Running,
        mem_mb,
        ..Default::default()
    }
}

fn job(id: i64, status: JobStatus, mem_mb: u64) -> Job {
    Job {
        id,
        status,
        device: (status != JobStatus::Queued).then(|| "mac".to_string()),
        spec: JobSpec {
            name: format!("j{id}"),
            command: vec!["git".into()],
            mem_mb,
            ..Default::default()
        },
        created_ms: id,
        ..Default::default()
    }
}

fn queued(id: i64) -> Job {
    job(id, JobStatus::Queued, 100)
}

fn cap(dv: &DeviceView, jobs: &[Job]) -> Capacity {
    let input = CapacityInput {
        now_ms: NOW,
        jobs,
        held: &[],
        paused: &[],
        quota: &[],
    };
    device_capacities(&input, std::slice::from_ref(dv)).remove(0)
}

/// The numbers a person reads: slots used/allowed, RAM used/allowed, state and reason.
fn read(c: &Capacity) -> (u32, Option<u32>, u64, Option<u64>, CapacityState, &str) {
    (
        c.slots_used,
        c.slots_allowed,
        c.mem_used_mib,
        c.mem_allowed_mib,
        c.state,
        &c.why,
    )
}

#[test]
fn a_device_with_room_and_an_empty_queue_is_idle_and_counts_what_runs() {
    let mut dv = device(run(2, 1024));
    dv.running = vec![running(1, 512), running(2, 512)];
    let jobs = [
        job(1, JobStatus::Running, 512),
        job(2, JobStatus::Running, 512),
    ];
    assert_eq!(
        read(&cap(&dv, &jobs)),
        (
            2,
            Some(4),
            1024,
            Some(2048),
            CapacityState::Idle,
            "queue empty"
        )
    );
}

#[test]
fn a_job_leased_but_not_started_already_holds_a_slot() {
    let dv = device(run(2, 1024));
    let jobs = [job(7, JobStatus::Leased, 512)];
    assert_eq!(
        read(&cap(&dv, &jobs)),
        (
            1,
            Some(2),
            512,
            Some(1024),
            CapacityState::Idle,
            "queue empty"
        )
    );
}

#[test]
fn a_device_with_no_free_slot_is_full_and_says_how_much_work_waits() {
    let mut dv = device(run(0, 4096));
    dv.running = vec![running(1, 100), running(2, 100)];
    let jobs = [
        job(1, JobStatus::Running, 100),
        job(2, JobStatus::Running, 100),
        queued(3),
    ];
    let c = cap(&dv, &jobs);
    assert_eq!(
        (c.state, c.slots_used, c.slots_allowed),
        (CapacityState::Full, 2, Some(2))
    );
    assert_eq!(c.why, "all 2 slots in use, 1 queued job waiting");
}

#[test]
fn ready_work_with_room_is_starting_and_a_job_too_big_for_the_budget_is_not() {
    let dv = device(run(2, 1024));
    let c = cap(&dv, &[queued(1), queued(2)]);
    assert_eq!(
        (c.state, c.why.as_str()),
        (
            CapacityState::Starting,
            "starting 2 queued jobs on the next heartbeat"
        )
    );
    let big = job(3, JobStatus::Queued, 4096);
    let c = cap(&dv, &[big]);
    assert_eq!(c.state, CapacityState::TooBig);
    assert_eq!(
        c.why,
        "the smallest queued job needs 4096 MiB but only 1024 MiB of the RAM budget is free"
    );
}

#[test]
fn the_guard_and_pauses_hold_a_device_under_the_reports_labels() {
    let mut memory = run(2, 1024);
    memory.action = GuardAction::Hold;
    memory.reasons = vec!["hold: memory pressure Warn".into()];
    let mut dv = device(memory);
    dv.running = vec![running(1, 256)];
    let jobs = [job(1, JobStatus::Running, 256), queued(2)];
    let c = cap(&dv, &jobs);
    assert_eq!(
        read(&c),
        (
            1,
            None,
            256,
            None,
            CapacityState::Held,
            "held: memory pressure"
        )
    );

    let mut paused = device(run(2, 1024));
    paused.paused = true;
    assert_eq!(cap(&paused, &[]).why, "held: a pause you set");
    let mut local = device(run(2, 1024));
    local.local_paused = true;
    assert_eq!(cap(&local, &[]).why, "held: a local pause");
}

#[test]
fn an_offline_device_says_so() {
    let mut dv = device(run(2, 1024));
    dv.online = false;
    let c = cap(&dv, &[]);
    assert_eq!(
        (c.state, c.why.as_str()),
        (CapacityState::Offline, "offline")
    );
}

#[test]
fn queued_jobs_that_cannot_run_here_or_wait_are_named_by_the_most_common_cause() {
    let dv = device(run(2, 1024));
    let needs = |id, tool: &str| {
        let mut j = queued(id);
        j.spec.requires = vec![tool.into()];
        j
    };
    let c = cap(
        &dv,
        &[needs(1, "docker"), needs(2, "docker"), needs(3, "cargo")],
    );
    assert_eq!(
        (c.state, c.why.as_str()),
        (
            CapacityState::CannotRun,
            "2 queued jobs cannot run here: no docker"
        )
    );

    let mut ai = queued(4);
    ai.spec.cost = crate::types::CostClass::Ai;
    ai.spec.ai_account = Some("claude".into());
    let mut dv = device(run(2, 1024));
    dv.info.ai_accounts = vec!["claude".into()];
    let c = cap(&dv, &[ai]);
    assert_eq!(
        (c.state, c.why.as_str()),
        (CapacityState::Waiting, "1 queued job waiting: AI quota")
    );

    let held = [queued(5), queued(6)];
    let input = CapacityInput {
        now_ms: NOW,
        jobs: &[],
        held: &held,
        paused: &[],
        quota: &[],
    };
    let c = device_capacities(&input, std::slice::from_ref(&dv)).remove(0);
    assert_eq!(
        (c.state, c.why.as_str()),
        (CapacityState::Waiting, "2 queued jobs paused by you")
    );
}

#[test]
fn paused_jobs_a_device_could_never_run_are_not_counted_for_it() {
    // An AI job held by an account pause: a laptop with the account could run it, a Linux
    // server (which takes no AI jobs) never could, so for the server the queue is just empty.
    let mut held = queued(9);
    held.spec.cost = crate::types::CostClass::Ai;
    held.spec.ai_account = Some("claude".into());
    let mut mac = device(run(2, 1024));
    mac.info.ai_accounts = vec!["claude".into()];
    let mut server = device(run(2, 1024));
    server.info.id = "srv".into();
    server.info.profile = crate::types::DeviceProfile::LinuxServer;
    let held = [held];
    let input = CapacityInput {
        now_ms: NOW,
        jobs: &[],
        held: &held,
        paused: &[],
        quota: &[],
    };
    let caps = device_capacities(&input, &[mac, server]);
    assert_eq!(caps[0].why, "1 queued job paused by you");
    assert_eq!(
        (caps[1].state, caps[1].why.as_str()),
        (CapacityState::Idle, "queue empty")
    );
}

#[test]
fn an_account_shows_the_window_that_blocks_it_and_the_quota_left_apart_from_may_spend() {
    let five = QuotaWindowKind::Rolling { minutes: 300 };
    let week = QuotaWindowKind::Weekly;
    // One quota row; every row of an account then carries the account-level verdict.
    let row = |window, state, headroom_pct, resets: Option<i64>| QuotaView {
        account: "claude".into(),
        window: Some(window),
        state,
        headroom_pct,
        resets_at_ms: resets,
        ..Default::default()
    };
    let verdict = |rows: Vec<QuotaView>, spendable: bool, why: &str| -> Vec<QuotaView> {
        rows.into_iter()
            .map(|mut v| {
                v.spendable = spendable;
                v.reasons = if spendable { vec![] } else { vec![why.into()] };
                v
            })
            .collect()
    };
    let read = |rows: &[QuotaView], paused: &[String]| {
        let a = accounts(rows, paused).remove(0);
        (
            a.headroom_pct,
            a.window,
            a.resets_at_ms,
            a.spendable,
            a.paused,
            a.why,
        )
    };
    let open = || {
        vec![
            row(five, WindowState::Active, 60.0, Some(5_000)),
            row(week, WindowState::Active, 30.0, Some(9_000)),
        ]
    };

    // May spend: the quota left is the least any window has, with that window's reset.
    assert_eq!(
        read(&verdict(open(), true, ""), &[]),
        (Some(30.0), Some(week), Some(9_000), true, false, None)
    );
    // Shut by a rule that is not a window (a lease): the quota left is still 30%, not 0%.
    let lease = "another AI job holds this account's lease";
    assert_eq!(
        read(&verdict(open(), false, lease), &["claude".to_string()]),
        (
            Some(30.0),
            Some(week),
            Some(9_000),
            false,
            true,
            Some(lease.into())
        )
    );
    // The week is used up while the 5-hour window has room: it is the week that blocks, and it
    // resets days after the 5-hour window does.
    let spent = vec![
        row(five, WindowState::Active, 40.0, Some(5_000)),
        row(week, WindowState::Active, 0.0, Some(9_000)),
    ];
    assert_eq!(
        read(&verdict(spent, false, "week: 99% used"), &[]),
        (
            Some(0.0),
            Some(week),
            Some(9_000),
            false,
            false,
            Some("week: 99% used".into())
        )
    );
    // Both are used up: the account is shut until the later reset.
    let both = vec![
        row(five, WindowState::Active, 0.0, Some(5_000)),
        row(week, WindowState::Active, 0.0, Some(9_000)),
    ];
    assert_eq!(read(&verdict(both, false, "x"), &[]).1, Some(week));
    // A window with no data blocks without a reset time, and says the quota left is unknown.
    let unknown = vec![
        row(five, WindowState::Active, 0.0, Some(5_000)),
        row(week, WindowState::NoData, 0.0, None),
    ];
    let (left, window, resets, ..) = read(&verdict(unknown, false, "week: no data"), &[]);
    assert_eq!((left, window, resets), (None, Some(week), None));
}
