//! Tests: the dispatch feed, the dispatcher step and the orchestrator pass, against an
//! in-memory store and over HTTP.

use idlemax_core::dispatch::inputs::RepoReport;
use idlemax_core::dispatch::project::ProjectView;
use reqwest::StatusCode;

use super::super::projects::project_views;
use super::super::tests::Api;
use super::*;

pub(crate) const T0: i64 = 1_800_000_000_000;

pub(crate) fn feed() -> DispatchInputs {
    let repo = |name: &str, working| RepoReport {
        path: format!("/code/{name}"),
        name: name.into(),
        default_branch: Some("main".into()),
        working,
        ..RepoReport::default()
    };
    DispatchInputs {
        device: "mac".into(),
        herdr: true,
        repos: vec![repo("app", true), repo("lib", false)],
    }
}

#[tokio::test]
async fn a_feed_adds_each_repository_once_off_and_marks_owner_activity() {
    let api = Api::start().await;
    for _ in 0..2 {
        let res = api.post("/v1/agent/dispatch-inputs", &feed()).await;
        assert_eq!(res.status(), StatusCode::OK);
    }
    let all: Vec<ProjectView> = api
        .get_as("/v1/projects", "localhost")
        .await
        .json()
        .await
        .unwrap();
    let shown: Vec<(&str, bool, bool)> = all
        .iter()
        .map(|v| (v.project.name.as_str(), v.project.enabled, v.owner_active))
        .collect();
    assert_eq!(shown, [("app", false, true), ("lib", false, false)]);
    assert_eq!(all[0].project.settings.base_branch.as_deref(), Some("main"));
}

#[test]
fn owner_activity_fades_and_mode_off_replies_with_no_project() {
    let mut store = Store::open_in_memory().unwrap();
    let cfg = Config::default();
    ingest_feed(&mut store, &cfg, &cfg, &feed(), T0).unwrap();
    let app = store.list_projects(None).unwrap()[0].project.id;
    store.set_project_enabled(app, true).unwrap();
    let active = |store: &Store, at| project_views(store, None, at).unwrap()[0].owner_active;
    assert!(active(&store, T0 + 60_000));
    assert!(!active(&store, T0 + 31 * 60_000));

    let reply = ingest_feed(&mut store, &cfg, &cfg, &feed(), T0).unwrap();
    assert_eq!(reply.projects.len(), 1);
    let mut off = Config::default();
    off.dispatcher.mode = Mode::Off;
    let reply = ingest_feed(&mut store, &cfg, &off, &feed(), T0).unwrap();
    assert!(reply.projects.is_empty());
}

#[test]
fn a_feed_without_herdr_is_named_in_the_plan_notes() {
    let mut store = Store::open_in_memory().unwrap();
    let cfg = Config::default();
    let mut inputs = feed();
    inputs.herdr = false;
    ingest_feed(&mut store, &cfg, &cfg, &inputs, T0).unwrap();
    let plan = gather_plan(&store, &cfg, &cfg, T0).unwrap().plan;
    assert!(
        plan.notes
            .contains(&"mac: herdr gave no folders".to_string()),
        "{:?}",
        plan.notes
    );
}

#[test]
fn a_step_queues_one_check_job_in_the_base_checkout() {
    use idlemax_core::dispatch::inputs::Checkout;
    use idlemax_core::dispatch::project::ProjectSettings;
    let mut store = Store::open_in_memory().unwrap();
    let cfg = Config::default();
    let mut inputs = feed();
    inputs.repos[1].base_commit = Some("c1".into());
    inputs.repos[1].checkout = Some(Checkout {
        path: "/state/checks/2".into(),
        commit: Some("c1".into()),
        error: None,
    });
    ingest_feed(&mut store, &cfg, &cfg, &inputs, T0).unwrap();
    let lib = store.list_projects(None).unwrap()[1].project.clone();
    store.set_project_enabled(lib.id, true).unwrap();
    let settings = ProjectSettings {
        base_branch: Some("main".into()),
        check_command: vec!["cargo".into(), "test".into()],
        ..Default::default()
    };
    store.set_project_settings(lib.id, &settings).unwrap();
    ingest_feed(&mut store, &cfg, &cfg, &inputs, T0).unwrap();

    let mut off = Config::default();
    off.dispatcher.mode = Mode::Off;
    crate::dispatch::step(&mut store, &cfg, &off, "", T0, false).unwrap();
    assert!(store.queued_jobs().unwrap().is_empty());

    crate::dispatch::step(&mut store, &cfg, &cfg, "", T0, false).unwrap();
    crate::dispatch::step(&mut store, &cfg, &cfg, "", T0 + 60_000, false).unwrap();
    let queued = store.queued_jobs().unwrap();
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(queued[0].spec.name, "dispatch-check:lib");
    assert_eq!(queued[0].spec.cwd.as_deref(), Some("/state/checks/2"));

    // The owner changes the check while the first job is still queued: the route drops
    // the record, and the next step queues a job with the new command.
    let changed = ProjectSettings {
        check_command: vec!["make".into(), "check".into()],
        ..settings
    };
    store.set_project_settings(lib.id, &changed).unwrap();
    store.delete_base_check(lib.id).unwrap();
    crate::dispatch::step(&mut store, &cfg, &cfg, "", T0 + 120_000, false).unwrap();
    let argv: Vec<Vec<String>> = store
        .queued_jobs()
        .unwrap()
        .into_iter()
        .map(|j| j.spec.command)
        .collect();
    assert!(
        argv.contains(&vec!["make".to_string(), "check".to_string()]),
        "{argv:?}"
    );
    let rec = store.base_checks().unwrap().pop().unwrap();
    let job = store.get_job(rec.job.unwrap()).unwrap().unwrap();
    assert_eq!(job.spec.command, ["make", "check"]);
}

/// A Claude account at 20 percent of its 5-hour window, a Mac with two open slots and a
/// switched-on project with two roadmap items; `at` re-sends the heartbeat so the device
/// stays online on the test clock.
pub(crate) fn pass_world() -> (Store, Config, impl Fn(&mut Store, UnixMs)) {
    use idlemax_core::dispatch::project::ProjectSettings;
    use idlemax_core::dispatch::roadmap::RoadmapItem;
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::quota::{rolling_5h, AccountConfig, Provider};
    use idlemax_core::types::{QuotaSnapshot, QuotaWindowKind};
    let mut cfg = Config::default();
    cfg.quota.accounts = vec![AccountConfig {
        name: "max".into(),
        provider: Provider::Claude,
        ..AccountConfig::default()
    }];
    // Spec 2's worker CLIs: an account no worker can start on is never offered (A21).
    cfg.workers.clis = vec!["claude".into()];
    let beat = move |store: &mut Store, at: UnixMs| {
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
        hb.quota = vec![
            snap(rolling_5h(), 20.0),
            snap(QuotaWindowKind::Weekly, 30.0),
        ];
        hb.room = Some(idlemax_core::dispatch::room::RoomDecision {
            state: idlemax_core::dispatch::room::LineState::Run,
            open_slots: 2,
            ..Default::default()
        });
        store.record_heartbeat(&hb, at).unwrap();
    };
    let mut store = Store::open_in_memory().unwrap();
    beat(&mut store, T0);
    let mut inputs = feed();
    inputs.repos.truncate(1);
    inputs.repos[0].working = false;
    inputs.repos[0].roadmap = ["Faster sync", "Dark mode"]
        .map(|t| RoadmapItem {
            title: t.into(),
            done_when: "it works".into(),
            ..Default::default()
        })
        .to_vec();
    ingest_feed(&mut store, &cfg, &cfg, &inputs, T0).unwrap();
    let id = store.list_projects(None).unwrap()[0].project.id;
    store.set_project_enabled(id, true).unwrap();
    let settings = ProjectSettings {
        base_branch: Some("main".into()),
        ..Default::default()
    };
    store.set_project_settings(id, &settings).unwrap();
    (store, cfg, beat)
}

fn passes(store: &Store) -> usize {
    let queued = store.queued_jobs().unwrap();
    queued
        .iter()
        .filter(|j| j.spec.name == "dispatch-orchestrator-pass")
        .count()
}

/// Leases the pass job and reports it succeeded with `envelope` as its output.
pub(crate) fn finish(store: &mut Store, cfg: &Config, envelope: &str, at: UnixMs) {
    use idlemax_core::protocol::JobUpdate;
    use idlemax_core::types::JobStatus;
    let job = store.queued_jobs().unwrap().pop().unwrap();
    let leased = store
        .lease_job(job.id, "mac", at + 60_000, at)
        .unwrap()
        .unwrap();
    let update = JobUpdate {
        seq: 1,
        id: job.id,
        device: "mac".into(),
        attempt: leased.attempts,
        status: JobStatus::Succeeded,
        at_ms: at,
        exit_code: Some(0),
        output_tail: Some(envelope.into()),
        ..Default::default()
    };
    let output = super::super::agents::on_job_update(store, &update, at).unwrap();
    crate::dispatch::ingest_pass(store, cfg, cfg, "rules", job.id, &output.unwrap(), at).unwrap();
}

#[test]
fn a_pass_runs_once_and_its_picks_and_judgments_last_until_things_change() {
    let (mut store, cfg, beat) = pass_world();
    let step = |store: &mut Store, at| {
        crate::dispatch::step(store, &cfg, &cfg, "rules", at, false).unwrap()
    };
    step(&mut store, T0);
    step(&mut store, T0 + 1_000);
    assert_eq!(passes(&store), 1);
    let envelope = serde_json::json!({"type": "result", "structured_output": {
        "picks": [{"task": "roadmap:1:faster-sync", "account": "max", "model": "sonnet",
                   "reason": "small and checked", "scope_paths": ["src/sync.rs"]}],
        "needs_you": [{"task": "roadmap:1:dark-mode", "reason": "too vague"}]}});
    finish(&mut store, &cfg, &envelope.to_string(), T0 + 2_000);

    let plan = gather_plan(&store, &cfg, &cfg, T0 + 3_000).unwrap().plan;
    let pick = &plan.devices[0].picks[0];
    assert_eq!(
        (pick.task.as_str(), pick.scope_paths.clone()),
        ("roadmap:1:faster-sync", vec!["src/sync.rs".to_string()])
    );
    assert_eq!(plan.needs_you[0].needs_you.as_deref(), Some("too vague"));
    assert!(plan.not_judged.is_empty(), "{:?}", plan.not_judged);

    for hours in [2, 4] {
        let at = T0 + hours * 3_600_000;
        beat(&mut store, at);
        step(&mut store, at);
    }
    assert_eq!(passes(&store), 0);
    assert_eq!(store.judgments().unwrap().len(), 1);

    let mut changed = feed();
    changed.repos.truncate(1);
    changed.repos[0].roadmap = ["Faster sync", "Dark mode, with a test"]
        .map(|t| idlemax_core::dispatch::roadmap::RoadmapItem {
            title: t.into(),
            done_when: "it works".into(),
            ..Default::default()
        })
        .to_vec();
    let at = T0 + 6 * 3_600_000;
    beat(&mut store, at);
    ingest_feed(&mut store, &cfg, &cfg, &changed, at).unwrap();
    step(&mut store, at);
    assert!(store.judgments().unwrap().is_empty());
    assert_eq!(passes(&store), 1);

    let id = store.list_projects(None).unwrap()[0].project.id;
    store.set_project_enabled(id, false).unwrap();
    let plan = gather_plan(&store, &cfg, &cfg, at).unwrap().plan;
    assert!(plan.devices[0].picks.is_empty());
}

/// Re-sends the readings of `max` with the 5-hour window at `five_pct`.
fn five_hour_at(store: &mut Store, at: UnixMs, five_pct: f64) {
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::quota::rolling_5h;
    use idlemax_core::types::{QuotaSnapshot, QuotaWindowKind};
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
    hb.quota = vec![
        snap(rolling_5h(), five_pct),
        snap(QuotaWindowKind::Weekly, 30.0),
    ];
    hb.room = Some(idlemax_core::dispatch::room::RoomDecision {
        state: idlemax_core::dispatch::room::LineState::Run,
        open_slots: 2,
        ..Default::default()
    });
    store.record_heartbeat(&hb, at).unwrap();
}

#[test]
fn a_pick_on_an_account_at_the_five_hour_stop_names_the_budgets_reason() {
    let (mut store, cfg, _beat) = pass_world();
    crate::dispatch::step(&mut store, &cfg, &cfg, "rules", T0, false).unwrap();
    let envelope = serde_json::json!({"type": "result", "structured_output": {
        "picks": [{"task": "roadmap:1:faster-sync", "account": "max", "model": "sonnet",
                   "reason": "small", "scope_paths": []}], "needs_you": []}});
    finish(&mut store, &cfg, &envelope.to_string(), T0 + 2_000);
    five_hour_at(&mut store, T0 + 3_000, 62.0);
    let plan = gather_plan(&store, &cfg, &cfg, T0 + 3_000).unwrap().plan;
    assert!(plan.devices[0].picks.is_empty());
    let reason = &plan.devices[0].empty.as_ref().unwrap().reason;
    assert!(
        reason.contains("5h window 62% used, stop line 60%"),
        "{reason}"
    );
}

#[test]
fn a_task_that_failed_its_checks_twice_needs_the_owner() {
    use idlemax_core::worker::run::{Run, RunLaunch, RunState, Verdict};
    let (mut store, cfg, _beat) = pass_world();
    for _ in 0..2 {
        let run = store
            .insert_run(
                &Run {
                    device: "mac".into(),
                    task: "roadmap:1:faster-sync".into(),
                    account: "max".into(),
                    start_ms: T0,
                    updated_ms: T0,
                    ..Run::default()
                },
                &RunLaunch::default(),
            )
            .unwrap();
        let rejected = Run {
            state: RunState::Rejected,
            verdict: Some(Verdict::Rejected),
            ..run
        };
        store.update_run(&rejected, RunState::Starting).unwrap();
    }
    crate::dispatch::step(&mut store, &cfg, &cfg, "rules", T0, false).unwrap();
    let plan = gather_plan(&store, &cfg, &cfg, T0).unwrap().plan;
    let judged = plan
        .needs_you
        .iter()
        .find(|t| t.id == "roadmap:1:faster-sync")
        .expect("judged");
    assert_eq!(judged.needs_you.as_deref(), Some("failed its checks twice"));
    assert!(!plan.not_judged.iter().any(|t| t.id == judged.id));
}

#[test]
fn a_pass_proposes_a_rule_for_a_rejected_run_and_it_waits_open() {
    use idlemax_core::dispatch::proposal::ProposalStatus;
    use idlemax_core::worker::run::{Run, RunLaunch, RunState};
    let (mut store, cfg, _) = pass_world();
    let run = Run {
        device: "mac".into(),
        project: "app".into(),
        task: "roadmap:1:old".into(),
        task_title: "Old task".into(),
        state: RunState::Rejected,
        concern: Some("deleted a fixture".into()),
        start_ms: T0,
        ..Run::default()
    };
    let run = store.insert_run(&run, &RunLaunch::default()).unwrap();
    crate::dispatch::step(&mut store, &cfg, &cfg, "rules", T0, false).unwrap();
    let pass = store.queued_jobs().unwrap().pop().unwrap();
    assert!(pass
        .spec
        .stdin
        .unwrap_or_default()
        .contains("deleted a fixture"));

    let envelope = serde_json::json!({"type": "result", "structured_output": {
        "picks": [], "needs_you": [],
        "rule_proposal": {"run": run.id, "line": "Never delete a fixture.",
                          "reason": "the run deleted one"}}});
    finish(&mut store, &cfg, &envelope.to_string(), T0 + 2_000);
    let all = store.proposals().unwrap();
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!((all[0].run, all[0].status), (run.id, ProposalStatus::Open));
    assert!(store.finished_runs().unwrap().is_empty());
}
