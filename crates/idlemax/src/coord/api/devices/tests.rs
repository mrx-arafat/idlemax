//! Tests: the device switch routes and what a switch-off stops, over HTTP and against an
//! in-memory store.

use idlemax_core::control::caller::{Link, Mark};
use idlemax_core::control::credential::{AgentAuth, Identity};
use idlemax_core::dispatch::room::{LineState, RoomDecision};
use idlemax_core::protocol::{ClusterStatus, Heartbeat, HeartbeatReply};
use idlemax_core::types::{Admission, JobSpec, JobStatus};
use idlemax_core::Config;
use reqwest::StatusCode;

use super::super::caller::Seen;
use super::super::tests::{mac_heartbeat, Api};
use super::*;

const T0: i64 = 1_800_000_000_000;

/// The Mac at `run` with two open worker slots.
fn roomy(id: &str) -> Heartbeat {
    let mut hb = mac_heartbeat(Vec::new());
    hb.device.id = id.into();
    hb.room = Some(RoomDecision {
        state: LineState::Run,
        open_slots: 2,
        ..Default::default()
    });
    hb
}

fn check_job(store: &mut Store) -> i64 {
    let spec = JobSpec {
        name: "dispatch-check:app".into(),
        command: vec!["true".into()],
        devices: vec!["mac".into()],
        admission: Admission::Lines,
        ..JobSpec::default()
    };
    store.insert_job(&spec, T0).unwrap().id
}

#[tokio::test]
async fn a_switch_answers_whether_it_changed_and_status_shows_it() {
    let api = Api::start().await;
    api.heartbeat(Vec::new()).await;
    for changed in [true, false] {
        let res = api.post("/v1/devices/mac/on", &()).await;
        assert_eq!(res.status(), StatusCode::OK);
        let reply: SwitchReply = res.json().await.unwrap();
        assert_eq!((reply.enabled, reply.changed), (true, changed));
    }
    let status: ClusterStatus = api
        .get_as("/v1/status", "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status.devices[0].enabled, Some(true));

    let res = api.post("/v1/devices/nope/on", &()).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(res.text().await.unwrap().contains("known devices are mac"));

    let res = api.post("/v1/agent/heartbeat", &roomy("my mac")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = api.post("/v1/devices/my%20mac/on", &()).await;
    assert_eq!(res.status(), StatusCode::OK);
    let reply: SwitchReply = res.json().await.unwrap();
    assert_eq!(reply.device, "my mac");
}

#[tokio::test]
async fn a_launched_caller_may_switch_a_device_off_but_not_on() {
    let marked: super::super::caller::Lookup = |_, _| Seen {
        same_host: true,
        chain: Some(vec![Link {
            pid: 20,
            mark: Mark::ThisHome,
        }]),
    };
    let api = Api::start_checked(Config::default(), Some(marked)).await;
    api.heartbeat(Vec::new()).await;
    let on = api.post("/v1/devices/mac/on", &()).await;
    assert_eq!(on.status(), StatusCode::FORBIDDEN);
    let off = api.post("/v1/devices/mac/off", &()).await;
    assert_eq!(off.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_switch_off_ends_the_dispatchers_jobs_and_an_off_device_leases_none() {
    let mut store = Store::open_in_memory().unwrap();
    store.record_heartbeat(&roomy("mac"), T0).unwrap();
    store.set_device_dispatch("mac", true, T0).unwrap();
    let check = check_job(&mut store);
    store.lease_job(check, "mac", T0 + 60_000, T0).unwrap();
    let plain = JobSpec {
        name: "build".into(),
        command: vec!["true".into()],
        ..JobSpec::default()
    };
    let plain = store.insert_job(&plain, T0).unwrap().id;
    store.lease_job(plain, "mac", T0 + 60_000, T0).unwrap();

    let reply = switch_device(&mut store, "mac", false, T0).unwrap();
    assert_eq!((reply.changed, reply.jobs_stopped), (true, 1));
    let status = |store: &Store, id| store.get_job(id).unwrap().unwrap().status;
    assert_eq!(status(&store, check), JobStatus::Killed);
    assert_eq!(status(&store, plain), JobStatus::Leased);
    assert!(store.paused_scopes().unwrap().is_empty());

    let queued = check_job(&mut store);
    // A second `off` changes nothing and stops nothing.
    let again = switch_device(&mut store, "mac", false, T0).unwrap();
    assert_eq!((again.changed, again.jobs_stopped), (false, 0));
    assert_eq!(status(&store, queued), JobStatus::Queued);
    let api = Api::start_with_store(store, Config::default()).await;
    let res = api.post("/v1/agent/heartbeat", &roomy("mac")).await;
    let reply: HeartbeatReply = res.json().await.unwrap();
    assert!(
        reply.assign.iter().all(|j| j.id != queued),
        "{:?}",
        reply.assign
    );
}

/// A worker job leased to the Mac and its Run in `running`.
fn running_worker(store: &mut Store, task: &str) -> i64 {
    use idlemax_core::worker::run::{Run, RunLaunch, RunState};
    use idlemax_core::worker::WorkerSpec;
    let spec = JobSpec {
        name: format!("worker:app:{task}"),
        command: vec!["true".into()],
        devices: vec!["mac".into()],
        ai_account: Some("max".into()),
        cost: idlemax_core::types::CostClass::Ai,
        worker: Some(WorkerSpec::default()),
        ..JobSpec::default()
    };
    let job = store.insert_job(&spec, T0).unwrap().id;
    store.lease_job(job, "mac", T0 + 60_000, T0).unwrap();
    let run = Run {
        device: "mac".into(),
        project: "app".into(),
        task: task.into(),
        state: RunState::Running,
        job: Some(job),
        start_ms: T0,
        updated_ms: T0,
        ..Run::default()
    };
    store.insert_run(&run, &RunLaunch::default()).unwrap();
    job
}

#[test]
fn a_switch_off_sheds_the_devices_workers_and_stores_no_owner_stop() {
    use idlemax_core::worker::run::RunState;
    let mut store = Store::open_in_memory().unwrap();
    store.record_heartbeat(&roomy("mac"), T0).unwrap();
    store.set_device_dispatch("mac", true, T0).unwrap();
    let workers = [
        running_worker(&mut store, "a"),
        running_worker(&mut store, "b"),
    ];
    let check = check_job(&mut store);
    store.lease_job(check, "mac", T0 + 60_000, T0).unwrap();
    let plain = JobSpec {
        name: "build".into(),
        command: vec!["true".into()],
        ..JobSpec::default()
    };
    let plain = store.insert_job(&plain, T0).unwrap().id;
    store.lease_job(plain, "mac", T0 + 60_000, T0).unwrap();

    let reply = switch_device(&mut store, "mac", false, T0).unwrap();
    assert_eq!((reply.runs_stopped, reply.jobs_stopped), (2, 1));
    let runs = store.list_runs(None, None, 0).unwrap();
    assert!(runs.iter().all(|r| r.state == RunState::Shed), "{runs:?}");
    let status = |id| store.get_job(id).unwrap().unwrap().status;
    for job in [workers[0], workers[1], check] {
        assert_eq!(status(job), JobStatus::Killed, "job {job}");
    }
    assert_eq!(status(plain), JobStatus::Leased);
    assert!(store.worker_stop().unwrap().is_none());
    assert!(store.paused_scopes().unwrap().is_empty());
}

fn credential_of(res: serde_json::Value) -> String {
    res["credential"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_device_credential_opens_only_its_own_agent_routes() {
    use reqwest::Method;
    let api = Api::start().await;
    let issued = api.post("/v1/devices/box/credential", &()).await;
    assert_eq!(issued.status(), StatusCode::OK);
    let cred = credential_of(issued.json().await.unwrap());
    let beat = |id: &str| serde_json::to_value(roomy(id)).unwrap();
    let send = |bearer: &str, method, path: &str, body: Option<serde_json::Value>| {
        let (api, bearer, path) = (&api, bearer.to_string(), path.to_string());
        async move {
            api.send_as(&bearer, method, &path, body.as_ref())
                .await
                .status()
        }
    };
    let hb = "/v1/agent/heartbeat";
    assert_eq!(
        send(&cred, Method::POST, hb, Some(beat("box"))).await,
        StatusCode::OK
    );
    assert_eq!(
        send(&cred, Method::POST, hb, Some(beat("mac"))).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&cred, Method::GET, "/v1/status", None).await,
        StatusCode::FORBIDDEN
    );
    let on = "/v1/devices/box/on";
    assert_eq!(
        send(&cred, Method::POST, on, None).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send("nope", Method::GET, "/v1/status", None).await,
        StatusCode::UNAUTHORIZED
    );

    let status: ClusterStatus = api
        .get_as("/v1/status", "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status.devices[0].agent_auth, Some(AgentAuth::Credential));
    api.heartbeat(Vec::new()).await;
    let status: ClusterStatus = api
        .get_as("/v1/status", "localhost")
        .await
        .json()
        .await
        .unwrap();
    let mac = status.devices.iter().find(|d| d.info.id == "mac").unwrap();
    assert_eq!(mac.agent_auth, Some(AgentAuth::OwnerTokenLocal));

    let second = credential_of(
        api.post("/v1/devices/box/credential", &())
            .await
            .json()
            .await
            .unwrap(),
    );
    assert_eq!(
        send(&cred, Method::POST, hb, Some(beat("box"))).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&second, Method::POST, hb, Some(beat("box"))).await,
        StatusCode::OK
    );
    let gone = api
        .send_as(
            super::super::tests::TOKEN,
            Method::DELETE,
            "/v1/devices/box/credential",
            None,
        )
        .await;
    assert_eq!(gone.status(), StatusCode::OK);
    assert_eq!(
        send(&second, Method::POST, hb, Some(beat("box"))).await,
        StatusCode::UNAUTHORIZED
    );
}

#[test]
fn the_store_keeps_the_digest_and_a_remote_owner_token_is_marked() {
    let mut store = Store::open_in_memory().unwrap();
    let reply = issue_credential(&mut store, "box", T0).unwrap();
    assert_eq!(store.credential_device(&reply.credential).unwrap(), None);
    let digest = super::super::http::credential_digest(&reply.credential);
    assert_eq!(
        store.credential_device(&digest).unwrap().as_deref(),
        Some("box")
    );

    let remote = crate::coord::server::Conn {
        peer: ([192, 0, 2, 1], 50000).into(),
        local: ([192, 0, 2, 2], 7878).into(),
    };
    let auth = super::super::agents::auth_of(&Identity::Owner, Some(remote));
    assert_eq!(auth, AgentAuth::OwnerTokenRemote);
}

#[test]
fn a_heartbeat_is_judged_by_how_it_proved_itself_this_time() {
    use idlemax_core::types::DeviceProfile;
    let mut store = Store::open_in_memory().unwrap();
    let mut hb = roomy("mac");
    hb.device.profile = DeviceProfile::Workbox;
    let cfg = Config::default();
    let beat = |store: &mut Store, auth| {
        super::super::agents::on_heartbeat(store, &cfg, &hb, auth, T0).unwrap()
    };
    beat(&mut store, AgentAuth::Credential);
    store.set_device_dispatch("mac", true, T0).unwrap();
    let job = check_job(&mut store);
    // The owner token from another host now: this beat leases nothing.
    let reply = beat(&mut store, AgentAuth::OwnerTokenRemote);
    assert!(
        reply.assign.iter().all(|j| j.id != job),
        "{:?}",
        reply.assign
    );
    let reply = beat(&mut store, AgentAuth::Credential);
    assert!(
        reply.assign.iter().any(|j| j.id == job),
        "{:?}",
        reply.assign
    );
}

/// Every agent route with a body that names another device; a new `/v1/agent/*` route must
/// be added here, or `every_agent_route_is_checked` fails.
const OTHER_DEVICE: [(&str, &str); 4] = [
    ("/v1/agent/heartbeat", r#"{"device": {"id": "mac"}}"#),
    (
        "/v1/agent/job-update",
        r#"{"device": "mac", "id": 1, "seq": 1}"#,
    ),
    ("/v1/agent/advisor-inputs", r#"{"device": "mac"}"#),
    (
        "/v1/agent/dispatch-inputs",
        r#"{"device": "mac", "herdr": false, "repos": []}"#,
    ),
];

#[tokio::test]
async fn a_device_credential_cannot_speak_for_another_device_on_any_agent_route() {
    let api = Api::start().await;
    let issued = api.post("/v1/devices/box/credential", &()).await;
    let cred = credential_of(issued.json().await.unwrap());
    for (path, body) in OTHER_DEVICE {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let res = api
            .send_as(&cred, reqwest::Method::POST, path, Some(&body))
            .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{path}");
    }
}

#[test]
fn every_agent_route_is_checked() {
    let router = include_str!("../../api.rs");
    let routes: Vec<&str> = router
        .split('"')
        .filter(|s| s.starts_with("/v1/agent/"))
        .collect();
    let checked: Vec<&str> = OTHER_DEVICE.iter().map(|(p, _)| *p).collect();
    assert_eq!(routes, checked);
}
