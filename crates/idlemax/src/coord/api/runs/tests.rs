//! Tests: the run routes, and the owner's stop and start of every worker.

use idlemax_core::worker::run::RunLaunch;
use idlemax_core::Config;
use reqwest::StatusCode;

use super::super::tests::Api;
use super::*;
use crate::coord::store::Store;

fn run(project: &str, state: RunState) -> Run {
    Run {
        device: "mac".into(),
        project: project.into(),
        task: "roadmap:1".into(),
        account: "max".into(),
        reason: "ready".into(),
        state,
        start_ms: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn runs_are_filtered_by_state_and_project_and_an_unknown_id_is_404() {
    let mut store = Store::open_in_memory().unwrap();
    let launch = RunLaunch {
        brief: "secret brief".into(),
        ..Default::default()
    };
    for (project, state) in [
        ("a", RunState::Starting),
        ("b", RunState::Starting),
        ("a", RunState::Clean),
    ] {
        store.insert_run(&run(project, state), &launch).unwrap();
    }
    let api = Api::start_with_store(store, Config::default()).await;
    let get = |path: &'static str| api.get_as(path, "localhost");

    let res = get("/v1/runs?state=starting&project=a").await;
    let text = res.text().await.unwrap();
    assert!(!text.contains("secret brief"), "{text}");
    let runs: Vec<Run> = serde_json::from_str(&text).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].id, runs[0].project.as_str()), (1, "a"));
    let all: Vec<Run> = get("/v1/runs").await.json().await.unwrap();
    assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), [3, 2, 1]);
    assert_eq!(
        get("/v1/runs?state=bogus").await.status(),
        StatusCode::BAD_REQUEST
    );

    let one: Run = get("/v1/runs/2").await.json().await.unwrap();
    assert_eq!(one.project, "b");
    let res = get("/v1/runs/999").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"], "run 999 not found");
}

/// Two live workers on account "max" and one on "work", each with a leased job.
fn live_workers(store: &mut Store) -> Vec<(Run, i64)> {
    use idlemax_core::types::{CostClass, JobSpec};
    use idlemax_core::worker::WorkerSpec;
    let mut out = Vec::new();
    for (i, (account, state)) in [
        ("max", RunState::Running),
        ("max", RunState::Starting),
        ("work", RunState::Running),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = run("app", state);
        r.account = account.into();
        r.task = format!("t{i}");
        let mut r = store.insert_run(&r, &RunLaunch::default()).unwrap();
        let spec = JobSpec {
            name: format!("w{i}"),
            command: vec!["idlemax-worker".into(), "claude".into()],
            cost: CostClass::Ai,
            ai_account: Some(account.into()),
            worker: Some(WorkerSpec {
                run: r.id,
                ..Default::default()
            }),
            ..Default::default()
        };
        let job = store
            .insert_leased_job(&spec, "mac", i64::MAX, 1)
            .unwrap()
            .into_job();
        r.job = Some(job.id);
        assert!(store.update_run(&r, state).unwrap());
        out.push((r, job.id));
    }
    out
}

async fn post_stop(api: &Api, scope: serde_json::Value) -> reqwest::Response {
    let body = serde_json::json!({"scope": scope, "reason": "going out"});
    api.post("/v1/workers/stop", &body).await
}

#[tokio::test]
async fn a_global_stop_stops_every_live_worker_even_under_a_pause_and_a_start_queues_them() {
    use idlemax_core::protocol::ControlRequest;
    use idlemax_core::types::{ControlAction, Job, JobStatus};
    let mut store = Store::open_in_memory().unwrap();
    let live = live_workers(&mut store);
    let api = Api::start_with_store(store, Config::default()).await;
    let pause = ControlRequest {
        action: ControlAction::Pause,
        scope: ControlScope::Global,
        reason: None,
    };
    assert_eq!(
        api.post("/v1/control", &pause).await.status(),
        StatusCode::OK
    );

    let res = post_stop(&api, serde_json::json!({"type": "global"})).await;
    assert_eq!(res.status(), StatusCode::OK);
    let reply: StopReply = res.json().await.unwrap();
    assert_eq!(reply.workers.len(), 3);
    assert!(reply.stop_in_force);
    for (r, job) in &live {
        let run: Run = api
            .get_as(&format!("/v1/runs/{}", r.id), "localhost")
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(run.state, RunState::Stopped);
        let job: Job = api
            .get_as(&format!("/v1/jobs/{job}"), "localhost")
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(job.status, JobStatus::Killed);
    }

    let res = api.post("/v1/workers/start", &()).await;
    assert_eq!(res.status(), StatusCode::OK);
    let reply: StartReply = res.json().await.unwrap();
    assert_eq!(reply.workers.len(), 3);
    assert_eq!(reply.paused, [ControlScope::Global]);
    let shed: Vec<Run> = api
        .get_as("/v1/runs?state=shed", "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(shed.len(), 3);
}

#[tokio::test]
async fn an_account_stop_sheds_only_its_workers_and_a_device_scope_is_refused() {
    let mut store = Store::open_in_memory().unwrap();
    let live = live_workers(&mut store);
    let api = Api::start_with_store(store, Config::default()).await;
    let res = post_stop(&api, serde_json::json!({"type": "account", "id": "max"})).await;
    let reply: StopReply = res.json().await.unwrap();
    assert_eq!(reply.workers.len(), 2);
    assert!(!reply.stop_in_force);
    let other: Run = api
        .get_as(&format!("/v1/runs/{}", live[2].0.id), "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(other.state, RunState::Running);
    let shed: Vec<Run> = api
        .get_as("/v1/runs?state=shed", "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(shed.len(), 2);
    let res = post_stop(&api, serde_json::json!({"type": "device", "id": "mac"})).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn the_owners_stop_survives_a_reopen_and_a_stop_all_run_waits_for_a_start() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("coord.db");
    {
        let mut store = Store::open(&db).unwrap();
        store
            .insert_run(&run("app", RunState::Stopped), &RunLaunch::default())
            .unwrap();
        let moved = plan_stop(&[], &ControlScope::Global, 5).unwrap();
        store.owner_stop(&moved, "night", 5).unwrap();
    }
    let mut store = Store::open(&db).unwrap();
    assert_eq!(
        store.worker_stop().unwrap().map(|(_, why)| why),
        Some("night".to_string())
    );
    // A Run a stop-all left stopped stays so until the owner's start.
    assert_eq!(
        store
            .list_runs(Some(RunState::Stopped), None, 0)
            .unwrap()
            .len(),
        1
    );
    let runs = store.list_runs(None, None, 0).unwrap();
    store.owner_start(&plan_start(&runs, 6)).unwrap();
    assert!(store.worker_stop().unwrap().is_none());
    assert_eq!(
        store
            .list_runs(Some(RunState::Shed), None, 0)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn keep_answers_404_and_409_and_a_succeeded_keep_leaves_the_run_kept() {
    use idlemax_core::protocol::JobUpdate;
    use idlemax_core::types::{Job, JobStatus};
    use idlemax_core::worker::run::Verdict;
    let mut store = Store::open_in_memory().unwrap();
    let live = live_workers(&mut store);
    let mut done = run("app", RunState::Clean);
    done.verdict = Some(Verdict::Clean);
    done.worktree = Some("/w/app/t".into());
    done.branch = Some("idlemax/t".into());
    let done = store.insert_run(&done, &RunLaunch::default()).unwrap();
    let api = Api::start_with_store(store, Config::default()).await;
    let keep = |id: i64| format!("/v1/runs/{id}/keep");
    let merge = serde_json::json!({"merge": false});

    let res = api.post(&keep(999), &merge).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("idlemax review"));
    assert_eq!(
        api.post(&keep(live[0].0.id), &merge).await.status(),
        StatusCode::CONFLICT
    );

    let res = api.post(&keep(done.id), &merge).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let job: Job = res.json().await.unwrap();
    assert_eq!(job.status, JobStatus::Leased);
    assert_eq!(
        api.post(&keep(done.id), &merge).await.status(),
        StatusCode::CONFLICT
    );
    let update = JobUpdate {
        seq: 1,
        id: job.id,
        device: "mac".into(),
        attempt: job.attempts,
        status: JobStatus::Succeeded,
        at_ms: 5,
        ..Default::default()
    };
    assert!(api
        .post("/v1/agent/job-update", &update)
        .await
        .status()
        .is_success());
    let after: Run = api
        .get_as(&format!("/v1/runs/{}", done.id), "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        (after.state, after.verdict),
        (RunState::Kept, Some(Verdict::Clean))
    );
}

#[tokio::test]
async fn a_leased_discard_reaches_a_paused_device_and_a_launch_does_not() {
    use idlemax_core::protocol::ControlRequest;
    use idlemax_core::types::ControlAction;
    let mut store = Store::open_in_memory().unwrap();
    let live = live_workers(&mut store);
    let shed = store
        .insert_run(&run("app", RunState::Shed), &RunLaunch::default())
        .unwrap();
    let api = Api::start_with_store(store, Config::default()).await;
    let pause = ControlRequest {
        action: ControlAction::Pause,
        scope: ControlScope::Global,
        reason: None,
    };
    assert_eq!(
        api.post("/v1/control", &pause).await.status(),
        StatusCode::OK
    );
    let res = api
        .post(&format!("/v1/runs/{}/discard", shed.id), &())
        .await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let discard: idlemax_core::types::Job = res.json().await.unwrap();
    let reply = api.heartbeat(Vec::new()).await;
    let sent: Vec<i64> = reply.assign.iter().map(|j| j.id).collect();
    assert!(sent.contains(&discard.id), "{sent:?}");
    assert!(!sent.contains(&live[0].1), "{sent:?}");
}
