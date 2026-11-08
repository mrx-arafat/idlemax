//! Tests: coordinator API bearer auth, stop_all and agent-sent advisor inputs over real
//! HTTP on loopback.

use idlemax_core::advice::{
    AdvisorInputs, GhItem, GithubSnapshot, GithubStatus, SessionMeta, SessionSignal,
};
use idlemax_core::explain::{ExplainState, Explanation, ReasonKind, WhySummary};
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
        Self::start_with(Config::default()).await
    }

    async fn start_with(config: Config) -> Self {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let app = AppState::new(store, config, Paths::new(home.path()));
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

    /// A GET that carries the token and the given `Host` header.
    async fn get_as(&self, path: &str, host: &str) -> reqwest::Response {
        self.http
            .get(format!("{}{path}", self.base))
            .header(reqwest::header::HOST, host)
            .bearer_auth(TOKEN)
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
async fn adding_the_same_key_twice_queues_one_job() {
    let api = Api::start().await;
    let spec = |key: &str| JobSpec {
        name: "tests".into(),
        key: Some(key.into()),
        command: vec!["true".into()],
        ..JobSpec::default()
    };

    let first = api.post("/v1/jobs", &spec("pr-7")).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: Job = first.json().await.unwrap();
    let again = api.post("/v1/jobs", &spec("pr-7")).await;
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(again.json::<Job>().await.unwrap().id, first.id);

    let other = api.post("/v1/jobs", &spec("pr-8")).await;
    assert_eq!(other.status(), StatusCode::CREATED);
    let blank = api.post("/v1/jobs", &spec("  ")).await;
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    let listed: Vec<Job> = api
        .http
        .get(format!("{}/v1/jobs", api.base))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
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

#[tokio::test]
async fn why_names_the_guard_reason_of_a_queued_job_and_404s_for_an_unknown_job() {
    let api = Api::start().await;
    let spec = JobSpec {
        name: "waiting".into(),
        command: vec!["true".into()],
        ..JobSpec::default()
    };
    let job: Job = api.post("/v1/jobs", &spec).await.json().await.unwrap();
    let mut hb = mac_heartbeat(Vec::new());
    hb.guard = GuardDecision {
        action: GuardAction::Pause,
        reasons: vec!["pause: on battery".into()],
        ..GuardDecision::default()
    };
    assert_eq!(
        api.post("/v1/agent/heartbeat", &hb).await.status(),
        StatusCode::OK
    );

    let get = |path: String| {
        api.http
            .get(format!("{}{path}", api.base))
            .bearer_auth(TOKEN)
            .send()
    };
    let why: Explanation = get(format!("/v1/jobs/{}/why", job.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(why.state, ExplainState::Blocked);
    let reason = &why.devices[0].reasons[0];
    assert_eq!(reason.kind, ReasonKind::Guard);
    assert!(reason
        .fix
        .as_deref()
        .unwrap_or_default()
        .contains("guard.require_ac_power"));

    let missing = get("/v1/jobs/999/why".into()).await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_result_replayed_after_its_lease_expired_still_finishes_the_job_once() {
    const T0: i64 = 1_800_000_000_000;
    let mut store = Store::open_in_memory().unwrap();
    let spec = JobSpec {
        name: "nightly".into(),
        command: vec!["true".into()],
        ..JobSpec::default()
    };
    let job = store.insert_job(&spec, T0).unwrap();
    let leased = store
        .lease_job(job.id, "mac", T0 + 600_000, T0)
        .unwrap()
        .unwrap();
    // The agent is down past the lease: the sweep puts the job back in the queue.
    let late = T0 + 700_000;
    let expired = idlemax_core::scheduler::expire_leases(&store.active_jobs().unwrap(), late);
    store.update_jobs(&expired).unwrap();
    assert_eq!(
        store.get_job(job.id).unwrap().unwrap().status,
        JobStatus::Queued
    );

    let update = JobUpdate {
        seq: 7,
        id: job.id,
        device: "mac".into(),
        attempt: leased.attempts,
        status: JobStatus::Succeeded,
        exit_code: Some(0),
        at_ms: T0 + 1_000,
        ..JobUpdate::default()
    };
    on_job_update(&mut store, &update, late + 1_000).unwrap();
    on_job_update(&mut store, &update, late + 2_000).unwrap();

    let done = store.get_job(job.id).unwrap().unwrap();
    assert_eq!(
        (done.status, done.device.as_deref(), done.exit_code),
        (JobStatus::Succeeded, Some("mac"), Some(0))
    );
    assert!(
        store.queued_jobs().unwrap().is_empty(),
        "it must not run again"
    );
    let succeeded = store
        .events(0)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::JobSucceeded)
        .count();
    assert_eq!(succeeded, 1);
}

#[test]
fn an_ai_pass_result_is_ingested_again_on_replay_until_its_advice_is_recorded() {
    const T0: i64 = 1_800_000_000_000;
    let mut store = Store::open_in_memory().unwrap();
    let spec = JobSpec {
        name: idlemax_core::advice::AI_PASS_JOB_NAME.into(),
        command: vec!["claude".into()],
        cost: CostClass::Ai,
        ai_account: Some("claude-max".into()),
        ..JobSpec::default()
    };
    let job = store.insert_job(&spec, T0).unwrap();
    store.lease_job(job.id, "mac", T0 + 600_000, T0).unwrap();
    let update = JobUpdate {
        seq: 5,
        id: job.id,
        device: "mac".into(),
        status: JobStatus::Succeeded,
        output_tail: Some("{\"result\":\"advice\"}".into()),
        at_ms: T0 + 1_000,
        ..JobUpdate::default()
    };

    // The coordinator stores the result, then dies before it ingests the output.
    let first = on_job_update(&mut store, &update, T0 + 2_000).unwrap();
    assert_eq!(first.as_deref(), update.output_tail.as_deref());
    // No ack was sent, so the agent replays it: the output must come back for ingest.
    let replay = on_job_update(&mut store, &update, T0 + 9_000).unwrap();
    assert_eq!(replay.as_deref(), update.output_tail.as_deref());

    // Once the ingest recorded its outcome for this job, a further replay is a no-op.
    let msg = "advice AI pass: 2 new, 0 closed, 0 job(s) queued, 0 stale cancelled";
    store
        .append_event(EventKind::Advice, None, Some(job.id), msg, T0 + 10_000)
        .unwrap();
    assert_eq!(
        on_job_update(&mut store, &update, T0 + 11_000).unwrap(),
        None
    );
}

#[tokio::test]
async fn why_summaries_cover_the_waiting_jobs_newest_first_with_one_headline_each() {
    let api = Api::start().await;
    let spec = |name: &str| JobSpec {
        name: name.into(),
        command: vec!["true".into()],
        ..JobSpec::default()
    };
    let mut ids = Vec::new();
    for name in ["old", "held", "new"] {
        let job: Job = api
            .post("/v1/jobs", &spec(name))
            .await
            .json()
            .await
            .unwrap();
        ids.push(job.id);
    }
    let finished = JobSpec {
        name: "done".into(),
        command: vec!["true".into()],
        ..JobSpec::default()
    };
    let done: Job = api.post("/v1/jobs", &finished).await.json().await.unwrap();
    api.post(&format!("/v1/jobs/{}/cancel", done.id), &json!({}))
        .await;
    let pause = json!({"scope": {"type": "job", "id": ids[1]}, "action": "pause"});
    api.post("/v1/control", &pause).await;
    let mut hb = mac_heartbeat(Vec::new());
    hb.guard = GuardDecision {
        action: GuardAction::Pause,
        reasons: vec!["pause: on battery".into()],
        ..GuardDecision::default()
    };
    api.post("/v1/agent/heartbeat", &hb).await;

    let get = |path: &str| {
        api.http
            .get(format!("{}{path}", api.base))
            .bearer_auth(TOKEN)
            .send()
    };
    let all: Vec<WhySummary> = get("/v1/why").await.unwrap().json().await.unwrap();
    // Newest first, the cancelled job left out, the paused one named by its pause.
    assert_eq!(
        all.iter().map(|s| s.job).collect::<Vec<_>>(),
        [ids[2], ids[1], ids[0]]
    );
    let kinds: Vec<ReasonKind> = all
        .iter()
        .map(|s| s.headline.as_ref().unwrap().reason.kind)
        .collect();
    assert_eq!(
        kinds,
        [ReasonKind::Guard, ReasonKind::JobPause, ReasonKind::Guard]
    );
    assert_eq!(
        all[0].headline.as_ref().unwrap().device.as_deref(),
        Some("mac")
    );

    let one: Vec<WhySummary> = get("/v1/why?limit=1").await.unwrap().json().await.unwrap();
    assert_eq!(one.iter().map(|s| s.job).collect::<Vec<_>>(), [ids[2]]);
}

/// DNS rebinding: a page that reaches 127.0.0.1:7878 under its own domain name sends that name as
/// `Host`. The token-free routes (dashboard, healthz) would answer it, so every route checks it.
#[tokio::test]
async fn a_foreign_host_name_gets_421_on_every_route_and_our_own_names_do_not() {
    let mut config = Config::default();
    config.coordinator.url = "http://box:7878".into();
    config.coordinator.allowed_hosts = vec!["server.tailnet.ts.net".into()];
    let api = Api::start_with(config).await;

    for path in ["/", "/healthz", "/v1/status", "/no/such/route"] {
        let res = api.get_as(path, "evil.example:7878").await;
        assert_eq!(res.status(), StatusCode::MISDIRECTED_REQUEST, "{path}");
    }
    let body: serde_json::Value = api
        .get_as("/healthz", "evil.example:7878")
        .await
        .json()
        .await
        .unwrap();
    let message = body["error"].as_str().unwrap();
    assert!(
        message.contains("evil.example") && message.contains("coordinator.allowed_hosts"),
        "{message}"
    );

    for host in [
        "127.0.0.1:7878",
        "localhost:7878",
        "[::1]:7878",
        "100.64.0.1:7878",
        "box:7878",
        "server.tailnet.ts.net",
    ] {
        for path in ["/healthz", "/v1/status"] {
            let res = api.get_as(path, host).await;
            assert_eq!(res.status(), StatusCode::OK, "{host} {path}");
        }
    }

    // HTTP/1.0 without a Host header is not a browser page: it is served.
    let mut raw = tokio::net::TcpStream::connect(api.base.trim_start_matches("http://"))
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut raw, b"GET /healthz HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    let mut reply = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut raw, &mut reply)
        .await
        .unwrap();
    assert!(
        reply.starts_with("HTTP/1.0 200") || reply.starts_with("HTTP/1.1 200"),
        "{reply}"
    );
}
