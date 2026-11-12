//! Tests: a device switched off sheds its workers and launches nothing, and switched on again
//! it relaunches them one at a time, through the launch step of dispatcher Spec 2.

use idlemax_core::dispatch::room::LineState;
use idlemax_core::types::JobStatus;
use idlemax_core::worker::run::{Run, RunState};

use super::tests::{beat, runs, sweep, world, DARK, FASTER};
use super::Refusals;
use crate::coord::api::switch_device;
use crate::coord::store::Store;

/// Marks the Run of `task` running, as its worker's first update would.
fn started(store: &mut Store, task: &str) -> Run {
    let run = runs(store).into_iter().find(|r| r.task == task).unwrap();
    let mut running = run.clone();
    running.state = RunState::Running;
    assert!(store.update_run(&running, run.state).unwrap());
    running
}

#[test]
fn an_off_device_sheds_its_workers_and_relaunches_them_one_at_a_time_once_on() {
    let (mut store, cfg, live) = world();
    let mut refusals = Refusals::default();
    let mut now = crate::coord::api::plan::tests::T0 + 10_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    let first = started(&mut store, FASTER);
    now += 61_000;
    beat(&mut store, now, 20.0, LineState::Run, &[first.job.unwrap()]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    let second = started(&mut store, DARK);

    let reply = switch_device(&mut store, "mac", false, now).unwrap();
    assert_eq!(reply.runs_stopped, 2);
    for run in [&first, &second] {
        let job = store.get_job(run.job.unwrap()).unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Killed);
    }
    let shed = |store: &Store| {
        runs(store)
            .iter()
            .filter(|r| r.state == RunState::Shed)
            .count()
    };
    // Off: the room has space, yet nothing is relaunched or started.
    now += 61_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    assert_eq!((shed(&store), runs(&store).len()), (2, 2));

    switch_device(&mut store, "mac", true, now).unwrap();
    now += 1_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    assert_eq!(shed(&store), 1, "one relaunch per sweep");
    sweep(&mut store, &cfg, &live, &mut refusals, now + 1_000);
    assert_eq!(shed(&store), 1, "relaunched before the first one started");
    let back = runs(&store)
        .into_iter()
        .find(|r| r.state == RunState::Starting)
        .unwrap();
    let back = started(&mut store, &back.task);
    now += 61_000;
    beat(&mut store, now, 20.0, LineState::Run, &[back.job.unwrap()]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    assert_eq!(shed(&store), 0);
    assert_eq!(
        runs(&store).len(),
        2,
        "a new task started before the shed ones"
    );
}

#[test]
fn a_shed_run_is_not_relaunched_on_a_device_whose_login_closed_its_account() {
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::quota::login::Login;
    use idlemax_core::types::ControlScope;
    let (mut store, cfg, live) = world();
    let mut refusals = Refusals::default();
    let mut now = crate::coord::api::plan::tests::T0 + 10_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    let first = started(&mut store, FASTER);
    super::stop_workers(
        &mut store,
        &ControlScope::Account("max".into()),
        "test",
        now,
    )
    .unwrap();

    // The same Mac, now logged out of Claude.
    now += 61_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    let mut hb: Heartbeat = store
        .device("mac", now, i64::MAX)
        .unwrap()
        .map(|d| Heartbeat {
            device: d.info,
            room: d.room,
            ..Heartbeat::default()
        })
        .unwrap();
    hb.device.claude_login = Some(Login::Out);
    store.record_heartbeat(&hb, now).unwrap();
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    let run = store.get_run(first.id).unwrap().unwrap();
    assert_eq!(run.state, RunState::Shed);
    let events = store.events(0).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.message.contains(&format!("run {}", first.id))
                && e.message.contains("not logged in")),
        "{:?}",
        events.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}
