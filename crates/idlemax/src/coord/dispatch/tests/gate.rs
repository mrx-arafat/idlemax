//! Tests: the worker gate of Spec 3 Task 6 in the `auto` step, with a Claude account `max`
//! and a Codex account `pro` on one Mac.

use idlemax_core::dispatch::room::{LineState, RoomDecision};
use idlemax_core::protocol::Heartbeat;
use idlemax_core::quota::budget::BudgetBasis;
use idlemax_core::quota::{rolling_5h, AccountConfig, Provider};
use idlemax_core::types::{QuotaSnapshot, QuotaWindowKind};

use super::*;

/// A heartbeat of the Mac at `at` with each account's 5-hour window at its percent.
fn beat_both(store: &mut Store, at: UnixMs, max_five: f64, pro_five: f64) {
    let snap = |account: &str, window, used| QuotaSnapshot {
        account: account.into(),
        window,
        used_pct: used,
        resets_at_ms: Some(at + 3_600_000),
        source: Default::default(),
        at_ms: at,
    };
    let mut hb = Heartbeat::default();
    hb.device.id = "mac".into();
    hb.device.ai_accounts = vec!["max".into(), "pro".into()];
    hb.device.capabilities = vec!["claude".into(), "codex".into()];
    hb.quota = vec![
        snap("max", rolling_5h(), max_five),
        snap("max", QuotaWindowKind::Weekly, 30.0),
        snap("pro", rolling_5h(), pro_five),
        snap("pro", QuotaWindowKind::Weekly, 30.0),
    ];
    hb.room = Some(RoomDecision {
        state: LineState::Run,
        reason: "memory 40%".into(),
        open_slots: 2,
        at_ms: at,
    });
    store.record_heartbeat(&hb, at).unwrap();
}

/// A pass that picked Faster sync on `max` and Dark mode on `pro`; Codex may review.
fn two_accounts() -> (Store, Config, Config) {
    let (mut store, mut cfg, _) = pass_world();
    cfg.quota.seed_standard_pct = 1.0;
    cfg.quota.codex_standard_model = Some("gpt-5".into());
    cfg.quota.accounts.push(AccountConfig {
        name: "pro".into(),
        provider: Provider::Codex,
        ..AccountConfig::default()
    });
    cfg.workers.clis = vec!["claude".into(), "codex".into()];
    cfg.workers.reviewers = vec!["codex".into()];
    beat_both(&mut store, T0, 20.0, 20.0);
    crate::dispatch::step(&mut store, &cfg, &cfg, "rules", T0, false).unwrap();
    let pick = |task: &str, account: &str, model: &str| {
        serde_json::json!({"task": task, "account": account, "model": model,
                           "reason": "next", "scope_paths": ["src/sync.rs"]})
    };
    let envelope = serde_json::json!({"type": "result", "structured_output": {
        "picks": [pick(FASTER, "max", "sonnet"), pick(DARK, "pro", "gpt-5")],
        "needs_you": []}});
    finish(&mut store, &cfg, &envelope.to_string(), T0 + 1_000);
    let mut live = cfg.clone();
    live.dispatcher.mode = Mode::Auto;
    (store, cfg, live)
}

#[test]
fn a_closed_account_is_passed_over_and_the_next_pick_launches_on_another() {
    let (mut store, cfg, live) = two_accounts();
    let shed = put_run(&mut store, "roadmap:1:old", RunState::Shed, T0);
    let now = T0 + 10_000;
    beat_both(&mut store, now, 65.0, 20.0);
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    assert_eq!(
        store.get_run(shed.id).unwrap().unwrap().state,
        RunState::Shed
    );
    let started: Vec<Run> = runs(&store)
        .into_iter()
        .filter(|r| r.state == RunState::Starting)
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(
        (started[0].account.as_str(), started[0].task.as_str()),
        ("pro", DARK)
    );
    let events = store.events(0).unwrap();
    let passed_over = format!("run {} not launched", shed.id);
    let why = events.iter().find(|e| e.message.starts_with(&passed_over));
    assert!(why.unwrap().message.contains("5h window"), "{events:#?}");
}

#[test]
fn a_reviewer_at_the_five_hour_stop_is_not_chosen() {
    let reviewer_of_the_max_run = |pro_five: f64| {
        let (mut store, cfg, live) = two_accounts();
        // Dark mode waits for review, so the pick on max is the one that launches.
        put_run(&mut store, DARK, RunState::Clean, T0);
        let now = T0 + 10_000;
        beat_both(&mut store, now, 20.0, pro_five);
        sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
        let run = runs(&store).into_iter().find(|r| r.task == FASTER).unwrap();
        let job = store.get_job(run.job.unwrap()).unwrap().unwrap();
        job.spec.worker.unwrap().reviewer.map(|r| r.account)
    };
    assert_eq!(reviewer_of_the_max_run(20.0).as_deref(), Some("pro"));
    assert_eq!(reviewer_of_the_max_run(65.0), None);
}

#[test]
fn a_reviewer_whose_budget_has_no_room_for_the_review_is_not_chosen() {
    let (mut store, mut cfg, mut live) = two_accounts();
    cfg.quota.seed_standard_pct = 0.95;
    live.quota.seed_standard_pct = 0.95;
    put_run(&mut store, DARK, RunState::Clean, T0);
    // Ten running Runs on pro commit 9.5 of its 10 measuring-week percent (A7).
    for i in 0..10 {
        let run = Run {
            device: "mac".into(),
            project: "other".into(),
            task: format!("other:{i}"),
            provider: Provider::Codex,
            account: "pro".into(),
            model: "gpt-5".into(),
            source: "roadmap".into(),
            state: RunState::Running,
            start_ms: T0,
            launches: 1,
            ..Default::default()
        };
        store.insert_run(&run, &RunLaunch::default()).unwrap();
    }
    let now = T0 + 10_000;
    beat_both(&mut store, now, 20.0, 20.0);
    let views = crate::coord::api::quota_views(&store, &cfg, now).unwrap();
    let pro = crate::coord::api::budgets(&store, &cfg, &live, &views, now).unwrap();
    let pro = pro.iter().find(|b| b.account == "pro").unwrap();
    assert!(pro.usable && pro.left_pct < 0.95, "{pro:?}");
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    let run = runs(&store).into_iter().find(|r| r.task == FASTER).unwrap();
    let job = store.get_job(run.job.unwrap()).unwrap().unwrap();
    assert_eq!(job.spec.worker.unwrap().reviewer, None);
}

#[test]
fn a_spent_budget_keeps_its_running_worker() {
    let (mut store, cfg, live) = world();
    // A finished Run took the weekly reading from 18 to the fixture's 30 at T0: 12 of the
    // measuring week's 10 percent, so nothing is left today.
    let mut hb = Heartbeat::default();
    hb.device.id = "mac".into();
    hb.quota = vec![QuotaSnapshot {
        account: "max".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: 18.0,
        resets_at_ms: Some(T0 + 3_600_000),
        source: Default::default(),
        at_ms: T0 - 7_200_000,
    }];
    store.record_heartbeat(&hb, T0 - 7_200_000).unwrap();
    let ended = Run {
        device: "mac".into(),
        project: "app".into(),
        task: "roadmap:1:done".into(),
        provider: Provider::Claude,
        account: "max".into(),
        model: "sonnet".into(),
        source: "roadmap".into(),
        state: RunState::Clean,
        start_ms: T0 - 7_200_000,
        end_ms: Some(T0),
        launches: 1,
        quota_before: vec![QuotaSnapshot {
            account: "max".into(),
            window: QuotaWindowKind::Weekly,
            used_pct: 18.0,
            resets_at_ms: Some(T0 + 3_600_000),
            source: Default::default(),
            at_ms: T0 - 7_200_000,
        }],
        ..Default::default()
    };
    store.insert_run(&ended, &RunLaunch::default()).unwrap();
    let running = put_run(&mut store, "roadmap:1:busy", RunState::Running, T0);
    let now = T0 + 10_000;
    super::beat(&mut store, now, 20.0, LineState::Run, &[]);
    let budget = |store: &Store| {
        let views = crate::coord::api::quota_views(store, &cfg, now).unwrap();
        crate::coord::api::budgets(store, &cfg, &live, &views, now).unwrap()[0].clone()
    };
    assert_eq!(budget(&store).basis, BudgetBasis::Spent);
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    let state = |store: &Store| store.get_run(running.id).unwrap().unwrap().state;
    assert_eq!(state(&store), RunState::Running);
}

#[test]
fn a_rest_the_update_did_not_act_on_is_stopped_by_the_next_sweep() {
    // The update path stored the hit, but the coordinator died before it stopped the workers.
    let (mut store, cfg, live) = world();
    let running = put_run(&mut store, "roadmap:1:busy", RunState::Running, T0);
    let now = T0 + 10_000;
    super::beat(&mut store, now, 20.0, LineState::Run, &[]);
    let writes = crate::coord::store::UpdateWrites {
        limit_hit: Some("max"),
        job: None,
        run: None,
        events: &[],
    };
    assert!(store.commit_update("mac", 9_001, &writes, now).unwrap());
    let views = crate::coord::api::quota_views(&store, &cfg, now).unwrap();
    let budgets = crate::coord::api::budgets(&store, &cfg, &live, &views, now).unwrap();
    assert_eq!(budgets[0].basis, BudgetBasis::Resting);
    sweep(
        &mut store,
        &cfg,
        &live,
        &mut Refusals::default(),
        now + 1_000,
    );
    let state = store.get_run(running.id).unwrap().unwrap().state;
    assert_eq!(state, RunState::Shed);
}

#[test]
fn a_shed_run_on_a_resting_account_waits_while_the_pick_launches_on_another() {
    let (mut store, cfg, live) = two_accounts();
    let shed = put_run(&mut store, "roadmap:1:old", RunState::Shed, T0);
    let now = T0 + 10_000;
    beat_both(&mut store, now, 20.0, 20.0);
    let writes = crate::coord::store::UpdateWrites {
        limit_hit: Some("max"),
        job: None,
        run: None,
        events: &[],
    };
    assert!(store.commit_update("mac", 9_001, &writes, now).unwrap());
    sweep(&mut store, &cfg, &live, &mut Refusals::default(), now);
    assert_eq!(
        store.get_run(shed.id).unwrap().unwrap().state,
        RunState::Shed
    );
    let started: Vec<Run> = runs(&store)
        .into_iter()
        .filter(|r| r.state == RunState::Starting)
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(
        (started[0].account.as_str(), started[0].task.as_str()),
        ("pro", DARK)
    );
}
