//! Tests: the device switch routes and what a switch-off stops, over HTTP and against an
//! in-memory store.

use idlemax_core::control::caller::{Link, Mark};
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
    let api = Api::start_with_store(store, Config::default()).await;
    let res = api.post("/v1/agent/heartbeat", &roomy("mac")).await;
    let reply: HeartbeatReply = res.json().await.unwrap();
    assert!(
        reply.assign.iter().all(|j| j.id != queued),
        "{:?}",
        reply.assign
    );
}
