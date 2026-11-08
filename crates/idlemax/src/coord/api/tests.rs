//! Tests: coordinator API bearer auth, stop_all and agent-sent advisor inputs over real
//! HTTP on loopback.

use idlemax_core::advice::{
    AdvisorInputs, GhItem, GithubSnapshot, GithubStatus, SessionMeta, SessionSignal,
};
use idlemax_core::protocol::{
    AgentCommand, ClusterStatus, Heartbeat, HeartbeatReply, JobUpdate, RunningJob,
};
use idlemax_core::types::{
    CostClass, DeviceInfo, DeviceProfile, EventKind, GuardAction, GuardDecision, Job, JobSpec,
    JobStatus, Os, Recommendation,
};
use idlemax_core::{Config, Paths};
use reqwest::StatusCode;
use serde_json::json;
use tokio::net::TcpListener;

use super::agents::on_job_update;
use super::{router, AppState};
use crate::coord::store::Store;
use crate::state::now_ms;

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
async fn stdin_is_private_in_operator_responses_but_reaches_the_agent() {
    let api = Api::start().await;
    let secret = "private stdin sentinel";
    let spec = JobSpec {
        name: "stdin-job".into(),
        command: vec!["cat".into()],
        stdin: Some(secret.into()),
        ..JobSpec::default()
    };

    let created = api.post("/v1/jobs", &spec).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert!(!created.text().await.unwrap().contains(secret));
    let listed = api
        .http
        .get(format!("{}/v1/jobs", api.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    assert!(!listed.text().await.unwrap().contains(secret));

    let reply = api.heartbeat(Vec::new()).await;
    assert_eq!(reply.assign[0].spec.stdin.as_deref(), Some(secret));
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

#[test]
fn a_replayed_job_update_changes_nothing_twice() {
    const T0: i64 = 1_800_000_000_000;
    let mut store = Store::open_in_memory().unwrap();
    let spec = JobSpec {
        name: "ai-pass".into(),
        command: vec!["true".into()],
        cost: CostClass::Ai,
        ai_account: Some("claude-max".into()),
        ..JobSpec::default()
    };
    let job = store.insert_job(&spec, T0).unwrap();
    store.lease_job(job.id, "mac", T0 + 600_000, T0).unwrap();
    let update = JobUpdate {
        seq: 7,
        id: job.id,
        device: "mac".into(),
        status: JobStatus::Failed,
        error: Some("usage limit".into()),
        rate_limited: true,
        at_ms: T0 + 1_000,
        ..JobUpdate::default()
    };

    // The agent sends it, is killed before the ack lands, and replays it after the restart.
    on_job_update(&mut store, &update, T0 + 2_000).unwrap();
    let after_first = store.get_job(job.id).unwrap().unwrap();
    on_job_update(&mut store, &update, T0 + 9_000).unwrap();

    assert_eq!(store.get_job(job.id).unwrap().unwrap(), after_first);
    let kinds: Vec<EventKind> = store.events(0).unwrap().iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds.len(),
        2,
        "one quota event and one transition: {kinds:?}"
    );
    assert_eq!(
        store.limit_hits(0).unwrap(),
        vec![("claude-max".into(), T0 + 2_000)]
    );
}

fn waiting_pr(number: u64) -> GhItem {
    let five_days_ago = now_ms() - 5 * 24 * 3_600_000;
    GhItem {
        repo: "owner/app".into(),
        number,
        title: format!("Change {number}"),
        created_ms: five_days_ago,
        updated_ms: five_days_ago,
        ..GhItem::default()
    }
}

fn mac_inputs(pr: u64) -> AdvisorInputs {
    AdvisorInputs {
        device: "mac".into(),
        github: GithubSnapshot {
            status: GithubStatus::Ok,
            review_requested: vec![waiting_pr(pr)],
            ..GithubSnapshot::default()
        },
        sessions: vec![SessionMeta {
            tool: "claude".into(),
            id: "abc123".into(),
            title: Some("Fix login".into()),
            project: Some("myproj".into()),
            last_activity_ms: now_ms() - 5 * 3_600_000,
            signals: vec![SessionSignal::TestsFailing],
        }],
    }
}

#[tokio::test]
async fn advisor_rules_fire_from_the_newest_inputs_an_agent_sent() {
    let api = Api::start().await;
    for pr in [1, 7] {
        let res = api.post("/v1/agent/advisor-inputs", &mac_inputs(pr)).await;
        assert_eq!(res.status(), StatusCode::OK);
    }
    let blank = AdvisorInputs::default();
    let res = api.post("/v1/agent/advisor-inputs", &blank).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let recs: Vec<Recommendation> = api
        .post("/v1/advice/refresh", &json!({}))
        .await
        .json()
        .await
        .unwrap();
    let titles: Vec<&str> = recs.iter().map(|r| r.title.as_str()).collect();
    assert!(
        titles.contains(&"Review owner/app#7: Change 7"),
        "titles: {titles:?}"
    );
    assert!(titles.contains(&"Session 'Fix login' in myproj still has failing tests"));
    assert!(!titles.iter().any(|t| t.contains("#1:")), "old copy kept");
}

#[tokio::test]
async fn schedules_queue_their_first_job_and_reject_duplicates_pause_and_remove() {
    use idlemax_core::schedule::{Decision, NewSchedule, ScheduleView};

    let api = Api::start().await;
    let spec = JobSpec {
        name: "nightly".into(),
        command: vec!["true".into()],
        stdin: Some("private prompt".into()),
        ..JobSpec::default()
    };
    let new = NewSchedule {
        spec,
        every: "1d".parse().unwrap(),
    };

    let added = api.post("/v1/schedules", &new).await;
    assert_eq!(added.status(), StatusCode::CREATED);
    let text = added.text().await.unwrap();
    assert!(!text.contains("private prompt"), "stdin stays private");
    let view: ScheduleView = serde_json::from_str(&text).unwrap();
    assert_eq!(view.schedule.every.to_string(), "1d");
    let job = view.schedule.last_job.expect("first job queued at once");
    assert_eq!(
        view.decision,
        Decision::Running {
            job,
            status: JobStatus::Queued
        }
    );

    let again = api.post("/v1/schedules", &new).await;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let body = again.text().await.unwrap();
    assert!(body.contains("idlemax schedule rm 1"), "{body}");
    let bad = json!({"spec": new.spec, "every": "soon"});
    assert_eq!(
        api.post("/v1/schedules", &bad).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let id = view.schedule.id;
    let paused: ScheduleView = api
        .post(&format!("/v1/schedules/{id}/pause"), &json!({}))
        .await
        .json()
        .await
        .unwrap();
    assert!(paused.schedule.paused);
    let missing = api.post("/v1/schedules/99/resume", &json!({})).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let removed = api
        .http
        .delete(format!("{}/v1/schedules/{id}", api.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    let listed: Vec<ScheduleView> = api
        .http
        .get(format!("{}/v1/schedules", api.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.is_empty());
    let jobs: Vec<Job> = api
        .http
        .get(format!("{}/v1/jobs", api.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "removing a schedule leaves its queued job");
}
