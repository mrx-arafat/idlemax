//! Tests: a completed slot asks for more work without waiting for the heartbeat timer.

use std::sync::atomic::{AtomicBool, Ordering};

use axum::{extract::State, routing::post, Json, Router};
use idlemax_core::config::{AgentConfig, Config, GuardPolicy};
use idlemax_core::protocol::{Heartbeat, HeartbeatReply, JobUpdate, JobUpdateReply};
use idlemax_core::quota::QuotaConfig;
use idlemax_core::types::{Job, JobSpec};
use idlemax_core::Paths;
use tokio::sync::oneshot;

use super::*;
use crate::agent::{metrics::Sampler, ring::MetricsRing};

#[derive(Clone)]
struct Coordinator {
    assigned: Arc<AtomicBool>,
    beats: mpsc::UnboundedSender<Heartbeat>,
}

async fn heartbeat(State(c): State<Coordinator>, Json(h): Json<Heartbeat>) -> Json<HeartbeatReply> {
    c.beats.send(h).unwrap();
    let assign = if c.assigned.swap(true, Ordering::SeqCst) {
        Vec::new()
    } else {
        vec![Job {
            id: 1,
            attempts: 1,
            spec: JobSpec {
                name: "short".into(),
                command: vec!["/bin/echo".into(), "finished".into()],
                mem_mb: 64,
                ..JobSpec::default()
            },
            ..Job::default()
        }]
    };
    Json(HeartbeatReply {
        assign,
        commands: Vec::new(),
    })
}

async fn update(Json(u): Json<JobUpdate>) -> Json<JobUpdateReply> {
    Json(JobUpdateReply {
        acked_seq: Some(u.seq),
    })
}

fn permissive_policy() -> GuardPolicy {
    GuardPolicy {
        max_jobs: 1,
        ram_budget_mib: 256,
        pause_on_thermal: false,
        mem_available_mib_pause: None,
        mem_available_mib_kill: None,
        swap_used_mib_pause: None,
        swap_used_mib_kill: None,
        swap_in_pages_per_sec_pause: None,
        psi_mem_some_pause: None,
        psi_mem_full_kill: None,
        psi_io_full_pause: None,
        psi_io_full_kill: None,
        load_per_cpu_pause: None,
        load_per_cpu_kill: None,
        load_1m_pause: None,
        load_1m_kill: None,
        mem_free_pct_pause: None,
        mem_free_pct_kill: None,
        swap_growth_pause: None,
        swap_growth_kill: None,
        disk_free_mib_pause: None,
        disk_free_mib_kill: None,
        ..GuardPolicy::linux_server()
    }
}

#[tokio::test]
async fn completion_refills_before_the_regular_heartbeat() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path());
    crate::state::ensure_dirs(&paths).unwrap();
    let (beats, mut received) = mpsc::unbounded_channel();
    let app = Router::new()
        .route("/v1/agent/heartbeat", post(heartbeat))
        .route("/v1/agent/job-update", post(update))
        .with_state(Coordinator {
            assigned: Arc::new(AtomicBool::new(false)),
            beats,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (stop_server, stopped_server) = oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped_server.await;
            })
            .await
            .unwrap();
    });
    let cfg = AgentConfig {
        command_allowlist: vec!["/bin/echo".into()],
        ..AgentConfig::default()
    };
    let probes = Probes::new(
        Sampler::new().unwrap(),
        MetricsRing::open(&paths.agent_state_dir(), 30).unwrap(),
        &paths,
        QuotaConfig::default(),
        cfg.clone(),
    );
    let (status, _status_rx) = watch::channel(LocalStatus::default());
    let agent = Agent::new(Wiring {
        device: DeviceInfo {
            id: "refill".into(),
            ..DeviceInfo::default()
        },
        guard: Guard::new(permissive_policy()),
        client: CoordClient::new(&url, "test").unwrap(),
        probes,
        runner: Runner::new(paths.clone(), "refill".into(), cfg),
        outbox: DurableOutbox::open(&paths.agent_state_dir(), 100, now_ms()),
        pause_file: paths.pause_file(),
        status,
        sample_every: Duration::from_secs(60),
        heartbeat_every: Duration::from_secs(60),
        advisor_inputs_every: None,
        device_refresh_every: Duration::from_secs(3600),
        notify: false,
        paths: paths.clone(),
        config: Config::default(),
    });
    let (_control, controls) = mpsc::unbounded_channel();
    let (_reload, reloads) = mpsc::unbounded_channel();
    let (stop_agent, stopped_agent) = oneshot::channel();
    let task = tokio::spawn(agent.run(
        async {
            let _ = stopped_agent.await;
        },
        controls,
        reloads,
    ));
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        received.recv().await.unwrap();
        received.recv().await.unwrap()
    })
    .await;
    let _ = stop_agent.send(());
    task.await.unwrap();
    let _ = stop_server.send(());
    server.await.unwrap();
    let refill = result.expect("a finished slot waited for the 60-second heartbeat");
    assert!(refill.running.is_empty());
    assert!(matches!(
        refill.guard.action,
        idlemax_core::types::GuardAction::Run { slots: 1, .. }
    ));
}
