//! Tests: base check re-running when a device's daily budget is spent.

use idlemax_core::control::ControlPlan;
use idlemax_core::dispatch::check::{BaseCheck, Phase};
use idlemax_core::dispatch::inputs::Checkout;
use idlemax_core::dispatch::project::{Project, ProjectSettings};
use idlemax_core::dispatch::task::{discover, TaskInput, TaskSource};
use idlemax_core::dispatch::Mode;
use idlemax_core::protocol::{Heartbeat, JobUpdate};
use idlemax_core::quota::{AccountConfig, Provider};
use idlemax_core::types::{ControlScope, JobStatus, UnixMs};
use idlemax_core::worker::run::{Run, RunLaunch, RunState};
use idlemax_core::Config;

use super::super::projects::project_views;
use super::tests::feed;
use super::*;
use crate::coord::store::Store;

const T0: UnixMs = 1_800_000_000_000;

fn spent_world() -> (Store, Config, Project) {
    let mut cfg = Config::default();
    cfg.quota.accounts = vec![AccountConfig {
        name: "oc".into(),
        provider: Provider::Opencode,
        daily_cap_runs: Some(1),
        ..AccountConfig::default()
    }];
    let mut store = Store::open_in_memory().unwrap();
    let mut hb = Heartbeat::default();
    hb.device.id = "workbox".into();
    hb.device.ai_accounts = vec!["oc".into()];
    hb.device.capabilities = vec!["opencode".into()];
    store.record_heartbeat(&hb, T0).unwrap();
    store.set_device_dispatch("workbox", true, T0).unwrap();

    let mut inputs = feed();
    inputs.device = "workbox".into();
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

    let run = store
        .insert_run(
            &Run {
                device: "workbox".into(),
                account: "oc".into(),
                model: "opencode".into(),
                source: "roadmap".into(),
                start_ms: T0 - 3_600_000,
                updated_ms: T0 - 3_600_000,
                ..Run::default()
            },
            &RunLaunch::default(),
        )
        .unwrap();
    store
        .update_run(
            &Run {
                state: RunState::Running,
                session: Some("s".into()),
                ..run
            },
            RunState::Starting,
        )
        .unwrap();

    (store, cfg, lib)
}

fn put_passed_check(store: &mut Store, proj: i64, hours_ago: i64) {
    store
        .put_base_check(&BaseCheck {
            project: proj,
            base_commit: "c1".into(),
            phase: Phase::Check,
            job: None,
            passed: Some(true),
            detail: Some("passed".into()),
            finished_ms: Some(T0 - hours_ago * 3_600_000),
        })
        .unwrap();
}

#[test]
fn spent_budget_rechecks_due_project_in_auto_mode() {
    let (mut store, cfg, lib) = spent_world();
    put_passed_check(&mut store, lib.id, 25);
    let mut auto = cfg.clone();
    auto.dispatcher.mode = Mode::Auto;

    crate::dispatch::step(&mut store, &cfg, &auto, "", T0, false).unwrap();
    let queued = store.queued_jobs().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].spec.name, "dispatch-check:lib");
    let key = queued[0].spec.key.clone().unwrap_or_default();
    assert!(key.contains(":c1:check:"), "same base commit: {key}");

    crate::dispatch::step(&mut store, &cfg, &auto, "", T0 + 60_000, false).unwrap();
    assert_eq!(store.queued_jobs().unwrap().len(), 1);
}

#[test]
fn recheck_queues_nothing_when_propose_or_paused_or_not_due() {
    let (mut store, cfg, lib) = spent_world();
    let mut auto = cfg.clone();
    auto.dispatcher.mode = Mode::Auto;

    put_passed_check(&mut store, lib.id, 25);
    crate::dispatch::step(&mut store, &cfg, &cfg, "", T0, false).unwrap();
    assert!(store.queued_jobs().unwrap().is_empty());

    put_passed_check(&mut store, lib.id, 23);
    crate::dispatch::step(&mut store, &cfg, &auto, "", T0, false).unwrap();
    assert!(store.queued_jobs().unwrap().is_empty());

    // An off device rechecks nothing either.
    put_passed_check(&mut store, lib.id, 25);
    store.set_device_dispatch("workbox", false, T0).unwrap();
    crate::dispatch::step(&mut store, &cfg, &auto, "", T0, false).unwrap();
    assert!(store.queued_jobs().unwrap().is_empty());
    store.set_device_dispatch("workbox", true, T0).unwrap();

    put_passed_check(&mut store, lib.id, 25);
    store
        .apply_control(
            &ControlPlan {
                pause: Some((ControlScope::Global, true)),
                jobs: Vec::new(),
                summary: String::new(),
            },
            T0,
        )
        .unwrap();
    crate::dispatch::step(&mut store, &cfg, &auto, "", T0, false).unwrap();
    assert!(store.queued_jobs().unwrap().is_empty());
}

#[test]
fn a_production_device_runs_no_base_check_and_the_plan_says_why() {
    use idlemax_core::types::DeviceProfile;
    let (mut store, cfg, lib) = spent_world();
    let mut hb = Heartbeat::default();
    hb.device.id = "workbox".into();
    hb.device.ai_accounts = vec!["oc".into()];
    hb.device.profile = DeviceProfile::Production;
    store.record_heartbeat(&hb, T0).unwrap();
    let mut auto = cfg.clone();
    auto.dispatcher.mode = Mode::Auto;

    // No record: Spec 1 would queue the setup or the check now.
    crate::dispatch::step(&mut store, &cfg, &auto, "", T0, false).unwrap();
    let names: Vec<String> = store
        .queued_jobs()
        .unwrap()
        .into_iter()
        .map(|j| j.spec.name)
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("dispatch-setup") || n.starts_with("dispatch-check")),
        "{names:?}"
    );
    let plan = gather_plan(&store, &cfg, &auto, T0).unwrap().plan;
    let note = format!(
        "{}: base checks do not run on a production device",
        lib.name
    );
    assert!(plan.notes.contains(&note), "{:?}", plan.notes);
}

#[test]
fn failed_recheck_becomes_broken_check_task() {
    let (mut store, cfg, lib) = spent_world();
    put_passed_check(&mut store, lib.id, 25);
    let mut auto = cfg.clone();
    auto.dispatcher.mode = Mode::Auto;

    crate::dispatch::step(&mut store, &cfg, &auto, "", T0, false).unwrap();
    let job = store.queued_jobs().unwrap().pop().unwrap();
    let leased = store
        .lease_job(job.id, "workbox", T0 + 60_000, T0)
        .unwrap()
        .unwrap();
    let update = JobUpdate {
        seq: 1,
        id: job.id,
        device: "workbox".into(),
        attempt: leased.attempts,
        status: JobStatus::Failed,
        at_ms: T0 + 60_000,
        exit_code: Some(1),
        error: Some("tests failed".into()),
        ..Default::default()
    };
    super::super::agents::on_job_update(&mut store, &update, T0 + 60_000).unwrap();
    crate::dispatch::step(&mut store, &cfg, &auto, "", T0 + 60_000, false).unwrap();

    let projects = project_views(&store, None, T0 + 60_000).unwrap();
    let repos = store.dispatch_inputs().unwrap();
    let checks = store.base_checks().unwrap();
    let tasks = discover(&TaskInput {
        now_ms: T0 + 60_000,
        projects: &projects,
        repos: &repos,
        checks: &checks,
        issues: &[],
        sessions: &[],
        judgments: &[],
        scoped: &[],
    });
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].source, TaskSource::BrokenCheck);
    assert_eq!(tasks[0].project, "lib");
}
