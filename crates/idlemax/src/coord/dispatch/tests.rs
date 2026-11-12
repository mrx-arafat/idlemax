//! Tests: the `auto` step against an in-memory store with one Mac, one Claude account, one
//! switched-on project and an orchestrator pass that picked its two roadmap items.

use idlemax_core::control::ControlPlan;
use idlemax_core::dispatch::room::{LineState, RoomDecision};
use idlemax_core::protocol::{Heartbeat, RunningJob};
use idlemax_core::quota::rolling_5h;
use idlemax_core::types::{JobStatus, QuotaSnapshot, QuotaWindowKind};
use idlemax_core::worker::WorkerAction;

use super::*;
use crate::coord::api::plan::tests::{finish, pass_world, T0};

const FASTER: &str = "roadmap:1:faster-sync";
const DARK: &str = "roadmap:1:dark-mode";

/// A heartbeat of the Mac at `at` with the 5-hour window at `five` percent, the room in
/// `state`, and the jobs it reports running.
fn beat(store: &mut Store, at: UnixMs, five: f64, state: LineState, running: &[i64]) {
    let snap = |window, used| QuotaSnapshot {
        account: "max".into(),
        window,
        used_pct: used,
        resets_at_ms: Some(at + 3_600_000),
        source: Default::default(),
        at_ms: at,
    };
    let mut hb = Heartbeat::default();
    hb.device.id = "mac".into();
    hb.device.ai_accounts = vec!["max".into()];
    hb.device.capabilities = vec!["claude".into()];
    hb.device.claude_login = Some(idlemax_core::quota::login::Login::In {
        id: Some("org-1".into()),
    });
    hb.quota = vec![
        snap(rolling_5h(), five),
        snap(QuotaWindowKind::Weekly, 30.0),
    ];
    hb.room = Some(RoomDecision {
        state,
        reason: "memory 70%".into(),
        open_slots: u32::from(state == LineState::Run) * 2,
        at_ms: at,
    });
    hb.running = running
        .iter()
        .map(|id| RunningJob {
            id: *id,
            status: JobStatus::Running,
            ..Default::default()
        })
        .collect();
    store.record_heartbeat(&hb, at).unwrap();
}

/// The world of Spec 1's pass tests with a pass that picked both roadmap items, and a live
/// config in `auto` with Claude workers allowed. A Run is estimated at 1 percent, so the
/// measuring week's 10 percent (A7) leaves room for the launches these tests are about.
fn world() -> (Store, Config, Config) {
    let (mut store, mut cfg, _) = pass_world();
    cfg.quota.seed_standard_pct = 1.0;
    crate::dispatch::step(&mut store, &cfg, &cfg, "rules", T0, false).unwrap();
    let pick = |task: &str, reason: &str| {
        serde_json::json!({"task": task, "account": "max", "model": "sonnet",
                           "reason": reason, "scope_paths": ["src/sync.rs"]})
    };
    let envelope = serde_json::json!({"type": "result", "structured_output": {
        "picks": [pick(FASTER, "small and checked"), pick(DARK, "next in the roadmap")],
        "needs_you": []}});
    finish(&mut store, &cfg, &envelope.to_string(), T0 + 1_000);
    let mut live = cfg.clone();
    live.dispatcher.mode = Mode::Auto;
    live.workers.clis = vec!["claude".into()];
    (store, cfg, live)
}

fn sweep(store: &mut Store, cfg: &Config, live: &Config, refusals: &mut Refusals, now: UnixMs) {
    let step = LaunchStep {
        cfg,
        live,
        rules: "rules",
        now,
    };
    launch_due(store, &step, refusals).unwrap();
}

fn runs(store: &Store) -> Vec<Run> {
    let mut all = store.list_runs(None, None, 0).unwrap();
    all.reverse();
    all
}

#[test]
fn the_first_sweep_launches_one_worker_and_the_second_waits_for_it() {
    let (mut store, cfg, live) = world();
    let mut refusals = Refusals::default();
    let now = T0 + 10_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    let all = runs(&store);
    assert_eq!(all.len(), 1);
    let run = &all[0];
    assert_eq!((run.state, run.task.as_str()), (RunState::Starting, FASTER));
    assert_eq!(run.reason, "small and checked");
    assert_eq!(run.quota_before.len(), 2);
    let job = store.get_job(run.job.unwrap()).unwrap().unwrap();
    assert_eq!(
        (job.status, job.device.as_deref()),
        (JobStatus::Leased, Some("mac"))
    );
    let worker = job.spec.worker.as_ref().unwrap();
    assert_eq!((worker.run, worker.action), (run.id, WorkerAction::Launch));
    assert_eq!(job.spec.command, ["idlemax-worker", "claude"]);
    assert!(job.spec.stdin.as_deref().unwrap().contains("Faster sync"));

    sweep(&mut store, &cfg, &live, &mut refusals, now + 1_000);
    assert_eq!(
        runs(&store).len(),
        1,
        "launched while the first was starting"
    );
    let mut running = run.clone();
    running.state = RunState::Running;
    assert!(store.update_run(&running, RunState::Starting).unwrap());
    sweep(&mut store, &cfg, &live, &mut refusals, now + 30_000);
    assert_eq!(runs(&store).len(), 1, "launched before memory settled");
    let later = now + 61_000;
    beat(&mut store, later, 20.0, LineState::Run, &[job.id]);
    sweep(&mut store, &cfg, &live, &mut refusals, later);
    let all = runs(&store);
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].task, DARK);
}

#[test]
fn nothing_is_launched_outside_the_fence() {
    type Block = Box<dyn Fn(&mut Store, &mut Config, UnixMs)>;
    let blocked: Vec<(&str, Block)> = vec![
        (
            "propose",
            Box::new(|_, live, _| live.dispatcher.mode = Mode::Propose),
        ),
        (
            "no worker cli",
            Box::new(|_, live, _| live.workers.clis.clear()),
        ),
        (
            "room hold",
            Box::new(|store, _, at| beat(store, at, 20.0, LineState::Hold, &[])),
        ),
        (
            "device paused",
            Box::new(|store, _, at| pause(store, ControlScope::Device("mac".into()), at)),
        ),
        (
            "account paused",
            Box::new(|store, _, at| pause(store, ControlScope::Account("max".into()), at)),
        ),
    ];
    for (what, block) in blocked {
        let (mut store, cfg, mut live) = world();
        let now = T0 + 10_000;
        beat(&mut store, now, 20.0, LineState::Run, &[]);
        block(&mut store, &mut live, now);
        sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
        assert!(runs(&store).is_empty(), "{what}");
        let workers = store
            .active_jobs()
            .unwrap()
            .into_iter()
            .filter(|j| j.spec.worker.is_some());
        assert_eq!(workers.count(), 0, "{what}");
    }
    // Offline: no heartbeat for longer than the offline limit.
    let (mut store, cfg, live) = world();
    beat(&mut store, T0, 20.0, LineState::Run, &[]);
    sweep(
        &mut store,
        &cfg,
        &live,
        &mut Refusals::default(),
        T0 + 10 * 60_000,
    );
    assert!(runs(&store).is_empty(), "offline");
}

fn pause(store: &mut Store, scope: ControlScope, at: UnixMs) {
    let plan = ControlPlan {
        pause: Some((scope, true)),
        jobs: Vec::new(),
        summary: "pause".into(),
    };
    store.apply_control(&plan, at).unwrap();
}

fn put_run(store: &mut Store, task: &str, state: RunState, at: UnixMs) -> Run {
    let run = Run {
        device: "mac".into(),
        project: "app".into(),
        task: task.into(),
        account: "max".into(),
        state,
        start_ms: at,
        launches: 1,
        ..Default::default()
    };
    store.insert_run(&run, &RunLaunch::default()).unwrap()
}

#[test]
fn a_project_at_its_review_cap_gets_no_launch() {
    let (mut store, cfg, live) = world();
    for (i, state) in [RunState::Clean, RunState::NeedsYou, RunState::Running]
        .into_iter()
        .enumerate()
    {
        put_run(&mut store, &format!("other:{i}"), state, T0);
    }
    let now = T0 + 10_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    assert_eq!(runs(&store).len(), 3);
    let events = store.events(0).unwrap();
    assert!(events.iter().any(|e| e.message.contains("the cap is 3")));
}

#[test]
fn a_finished_task_is_not_launched_again_and_a_discarded_one_is() {
    let (mut store, cfg, live) = world();
    put_run(&mut store, FASTER, RunState::Clean, T0);
    let now = T0 + 10_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    assert_eq!(runs(&store)[1].task, DARK);

    let (mut store, cfg, live) = world();
    put_run(&mut store, FASTER, RunState::Discarded, T0);
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    assert_eq!(runs(&store)[1].task, FASTER);

    let (mut store, cfg, live) = world();
    put_run(&mut store, FASTER, RunState::Rejected, T0);
    put_run(&mut store, FASTER, RunState::Rejected, T0 + 1);
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    let all = runs(&store);
    assert_eq!(all.last().unwrap().task, DARK);
    assert_eq!(all.iter().filter(|r| r.task == FASTER).count(), 2);
}

#[test]
fn a_spent_account_sheds_its_worker_which_resumes_when_the_device_let_it_go() {
    let (mut store, cfg, live) = world();
    // The other roadmap item waits for review, so the shed Run is the only candidate.
    put_run(&mut store, DARK, RunState::Clean, T0);
    let mut refusals = Refusals::default();
    let now = T0 + 10_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut refusals, now);
    let first = runs(&store).remove(1);
    assert_eq!(first.task, FASTER);
    let job = first.job.unwrap();
    let mut running = first.clone();
    running.state = RunState::Running;
    running.session = Some("sess-1".into());
    assert!(store.update_run(&running, RunState::Starting).unwrap());

    let hot = now + 61_000;
    beat(&mut store, hot, 60.0, LineState::Run, &[job]);
    sweep(&mut store, &cfg, &live, &mut refusals, hot);
    assert_eq!(
        store.get_run(first.id).unwrap().unwrap().state,
        RunState::Shed
    );
    assert_eq!(
        store.get_job(job).unwrap().unwrap().status,
        JobStatus::Killed
    );
    assert_eq!(
        runs(&store).len(),
        2,
        "a new task started on a spent account"
    );

    // The window cools down, but the device still reports the old worker.
    let cool = hot + 61_000;
    beat(&mut store, cool, 20.0, LineState::Run, &[job]);
    sweep(&mut store, &cfg, &live, &mut refusals, cool);
    assert_eq!(
        store.get_run(first.id).unwrap().unwrap().state,
        RunState::Shed
    );
    assert_eq!(
        runs(&store).len(),
        2,
        "a new task started before the shed one"
    );

    beat(&mut store, cool + 1_000, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut refusals, cool + 1_000);
    let again = store.get_run(first.id).unwrap().unwrap();
    assert_eq!((again.state, again.launches), (RunState::Starting, 2));
    let next = store.get_job(again.job.unwrap()).unwrap().unwrap();
    let worker = next.spec.worker.unwrap();
    assert_eq!(worker.action, WorkerAction::Resume);
    assert_eq!(worker.session.as_deref(), Some("sess-1"));
    assert_eq!(runs(&store).len(), 2);
}

#[test]
fn a_slot_refused_on_ten_sweeps_for_one_reason_leaves_one_event() {
    // The project is at its review cap: with no worker CLI the plan would drop the picks
    // before a launch could refuse them (Spec 3 Task 8).
    let (mut store, cfg, live) = world();
    for (i, state) in [RunState::Clean, RunState::NeedsYou, RunState::Running]
        .into_iter()
        .enumerate()
    {
        put_run(&mut store, &format!("other:{i}"), state, T0);
    }
    let mut refusals = Refusals::default();
    for i in 0..10 {
        let at = T0 + 10_000 + i * 15_000;
        beat(&mut store, at, 20.0, LineState::Run, &[]);
        sweep(&mut store, &cfg, &live, &mut refusals, at);
    }
    let events = store.events(0).unwrap();
    let refused = events
        .iter()
        .filter(|e| e.message.starts_with("app: Faster sync not launched"))
        .count();
    assert_eq!(refused, 1);
}

#[test]
fn nothing_is_launched_while_the_owners_stop_is_stored() {
    let (mut store, cfg, live) = world();
    store.owner_stop(&[], "night", T0).unwrap();
    let now = T0 + 10_000;
    beat(&mut store, now, 20.0, LineState::Run, &[]);
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    assert!(runs(&store).is_empty());
    store.owner_start(&[]).unwrap();
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    assert_eq!(runs(&store).len(), 1);
}

mod gate;
