//! Tests: the coordinator's handling of worker jobs and Runs over real HTTP on loopback.

use idlemax_core::protocol::{ControlRequest, JobUpdate};
use idlemax_core::types::{ControlAction, ControlScope, CostClass, Job, JobSpec, JobStatus};
use idlemax_core::worker::report::{WorkerPhase, WorkerReport};
use idlemax_core::worker::run::{Run, RunLaunch, RunState};
use idlemax_core::worker::WorkerSpec;
use idlemax_core::Config;
use reqwest::StatusCode;
use serde_json::json;

use super::agents::on_job_update;
use super::tests::Api;
use crate::coord::store::Store;

#[tokio::test]
async fn worker_spec_is_refused_from_the_api() {
    let api = Api::start().await;
    let spec = json!({
        "name": "sneaky",
        "command": ["idlemax-worker", "claude"],
        "worker": {"run": 1, "project": "demo"},
    });
    let res = api.post("/v1/jobs", &spec).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(
        body["error"], "worker is set by the dispatcher only",
        "{body}"
    );
}

const T0: i64 = 1_800_000_000_000;

/// A store with one Run in `state` and its worker job leased to "mac" on account "max".
fn store_with_worker(state: RunState) -> (Store, Run, Job) {
    let mut store = Store::open_in_memory().unwrap();
    let run = Run {
        device: "mac".into(),
        account: "max".into(),
        state,
        start_ms: T0,
        ..Default::default()
    };
    let mut run = store.insert_run(&run, &RunLaunch::default()).unwrap();
    let spec = JobSpec {
        name: "w".into(),
        command: vec!["idlemax-worker".into(), "claude".into()],
        cost: CostClass::Ai,
        ai_account: Some("max".into()),
        worker: Some(WorkerSpec {
            run: run.id,
            ..Default::default()
        }),
        ..Default::default()
    };
    let job = store
        .insert_leased_job(&spec, "mac", i64::MAX, T0)
        .unwrap()
        .into_job();
    run.job = Some(job.id);
    assert!(store.update_run(&run, state).unwrap());
    (store, run, job)
}

fn running_update(job: &Job, seq: u64) -> JobUpdate {
    JobUpdate {
        seq,
        id: job.id,
        device: "mac".into(),
        attempt: job.attempts,
        status: JobStatus::Running,
        at_ms: T0,
        worker: Some(Box::new(WorkerReport {
            run: job.spec.worker.as_ref().unwrap().run,
            phase: WorkerPhase::Running,
            session: Some("sess".into()),
            ..Default::default()
        })),
        ..Default::default()
    }
}

#[test]
fn a_job_update_moves_the_run_once_and_a_replay_changes_nothing() {
    let (mut store, run, job) = store_with_worker(RunState::Starting);
    on_job_update(&mut store, &running_update(&job, 5), T0).unwrap();
    let mut after = store.get_run(run.id).unwrap().unwrap();
    assert_eq!(after.state, RunState::Running);
    assert_eq!(after.session.as_deref(), Some("sess"));

    after.state = RunState::Starting;
    assert!(store.update_run(&after, RunState::Running).unwrap());
    on_job_update(&mut store, &running_update(&job, 5), T0).unwrap();
    assert_eq!(
        store.get_run(run.id).unwrap().unwrap().state,
        RunState::Starting
    );
}

#[test]
fn an_update_never_moves_a_run_the_owner_stopped() {
    let (mut store, run, job) = store_with_worker(RunState::Stopped);
    on_job_update(&mut store, &running_update(&job, 6), T0).unwrap();
    assert_eq!(
        store.get_run(run.id).unwrap().unwrap().state,
        RunState::Stopped
    );
}

#[tokio::test]
async fn stop_all_on_the_account_kills_the_job_and_stops_the_run() {
    let (mut store, run, job) = store_with_worker(RunState::Starting);
    on_job_update(&mut store, &running_update(&job, 7), T0).unwrap();
    let config =
        Config::parse("[[quota.accounts]]\nname = \"max\"\nprovider = \"claude\"\n").unwrap();
    let api = Api::start_with_store(store, config).await;
    let req = ControlRequest {
        scope: ControlScope::Account("max".into()),
        action: ControlAction::StopAll,
        reason: None,
    };
    assert_eq!(api.post("/v1/control", &req).await.status(), StatusCode::OK);
    let stored: Job = api
        .get_as(&format!("/v1/jobs/{}", job.id), "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(stored.status, JobStatus::Killed);
    let stored: Run = api
        .get_as(&format!("/v1/runs/{}", run.id), "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(stored.state, RunState::Stopped);
}
