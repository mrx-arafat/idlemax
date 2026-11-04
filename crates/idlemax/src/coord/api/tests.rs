//! Tests: coordinator API bearer auth and stop_all over real HTTP on loopback.

use idlemax_core::protocol::{
    AgentCommand, ClusterStatus, Heartbeat, HeartbeatReply, JobUpdate, RunningJob,
};
use idlemax_core::types::{
    DeviceInfo, DeviceProfile, GuardAction, GuardDecision, Job, JobSpec, JobStatus, Os,
};
use idlemax_core::{Config, Paths};
use reqwest::StatusCode;
use serde_json::json;
use tokio::net::TcpListener;

use super::{router, AppState};
use crate::coord::store::Store;

const TOKEN: &str = "coord-t0ken";

struct Api {
    base: String,
    http: reqwest::Client,
    _home: tempfile::TempDir,
}

impl Api {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let app = AppState::new(store, Config::default(), Paths::new(home.path()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(app, TOKEN)).await });
        Self {
            base,
            http: reqwest::Client::new(),
            _home: home,
        }
    }

    async fn post(&self, path: &str, body: &impl serde::Serialize) -> reqwest::Response {
        let url = format!("{}{path}", self.base);
        self.http
            .post(url)
            .bearer_auth(TOKEN)
            .json(body)
            .send()
            .await
            .unwrap()
    }

    async fn heartbeat(&self, running: Vec<RunningJob>) -> HeartbeatReply {
        let res = self
            .post("/v1/agent/heartbeat", &mac_heartbeat(running))
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        res.json().await.unwrap()
    }
}

fn mac_heartbeat(running: Vec<RunningJob>) -> Heartbeat {
    let run = GuardAction::Run {
        slots: 2,
        mem_mib: 1024,
    };
    Heartbeat {
        device: DeviceInfo {
            id: "mac".into(),
            hostname: "mac".into(),
            os: Os::Macos,
            cpu_count: 10,
            profile: DeviceProfile::MacosLaptop,
            ..DeviceInfo::default()
        },
        guard: GuardDecision {
            action: run,
            resource_action: run,
            ..GuardDecision::default()
        },
        running,
        ..Heartbeat::default()
    }
}

#[tokio::test]
async fn bearer_token_is_required_and_stop_all_kills_on_the_next_heartbeat() {
    let api = Api::start().await;
    let anon = |path: &str| api.http.get(format!("{}{path}", api.base)).send();
    assert_eq!(anon("/healthz").await.unwrap().text().await.unwrap(), "ok");
    assert_eq!(
        anon("/v1/status").await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let spec = JobSpec {
        name: "health".into(),
        command: vec!["true".into()],
        ..JobSpec::default()
    };
    let job: Job = api.post("/v1/jobs", &spec).await.json().await.unwrap();
    let reply = api.heartbeat(Vec::new()).await;
    let leased: Vec<(i64, JobStatus)> = reply.assign.iter().map(|j| (j.id, j.status)).collect();
    assert_eq!(leased, vec![(job.id, JobStatus::Leased)]);
    let update = JobUpdate {
        id: job.id,
        device: "mac".into(),
        status: JobStatus::Running,
        ..JobUpdate::default()
    };
    assert_eq!(
        api.post("/v1/agent/job-update", &update).await.status(),
        StatusCode::OK
    );

    let stop = json!({"scope": {"type": "global"}, "action": "stop_all", "reason": "test"});
    let status: ClusterStatus = api.post("/v1/control", &stop).await.json().await.unwrap();
    assert!(status.global_paused);
    assert_eq!(status.jobs.killed, 1);
    let running = RunningJob {
        id: job.id,
        status: JobStatus::Running,
        ..RunningJob::default()
    };
    let reply = api.heartbeat(vec![running]).await;
    assert_eq!(reply.commands, vec![AgentCommand::KillJob { id: job.id }]);
    assert!(reply.assign.is_empty());
}
