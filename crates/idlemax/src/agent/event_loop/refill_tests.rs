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
    job: Option<Job>,
    updates: Option<mpsc::UnboundedSender<JobUpdate>>,
}

async fn heartbeat(State(c): State<Coordinator>, Json(h): Json<Heartbeat>) -> Json<HeartbeatReply> {
    c.beats.send(h).unwrap();
    let assign = if c.assigned.swap(true, Ordering::SeqCst) {
        Vec::new()
    } else {
        vec![c.job.clone().unwrap_or_else(|| Job {
            id: 1,
            attempts: 1,
            spec: JobSpec {
                name: "short".into(),
                command: vec!["/bin/echo".into(), "finished".into()],
                mem_mb: 64,
                ..JobSpec::default()
            },
            ..Job::default()
        })]
    };
    Json(HeartbeatReply {
        assign,
        commands: Vec::new(),
        queued_work: false,
        notices: Vec::new(),
    })
}

async fn update(State(c): State<Coordinator>, Json(u): Json<JobUpdate>) -> Json<JobUpdateReply> {
    if let Some(updates) = &c.updates {
        let _ = updates.send(u.clone());
    }
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
        ..GuardPolicy::production()
    }
}

#[tokio::test]
async fn completion_refills_before_the_regular_heartbeat() {
    run_loop(false, false).await;
}

#[tokio::test]
async fn hung_cwd_fails_start_handles_control_and_ignores_late_success() {
    run_loop(true, false).await;
}

#[tokio::test]
async fn hung_cwd_does_not_prevent_a_healthy_job_starting_in_the_same_batch() {
    run_loop(true, true).await;
}

#[tokio::test]
async fn jobs_with_the_same_healthy_cwd_both_start_from_one_check() {
    run_loop(false, true).await;
}

async fn run_loop(hung_cwd: bool, isolation: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path());
    crate::state::ensure_dirs(&paths).unwrap();
    let (beats, mut received) = mpsc::unbounded_channel();
    let (updates, mut updated) = mpsc::unbounded_channel();
    let app = Router::new()
        .route("/v1/agent/heartbeat", post(heartbeat))
        .route("/v1/agent/job-update", post(update))
        .with_state(Coordinator {
            assigned: Arc::new(AtomicBool::new(false)),
            beats,
            job: hung_cwd.then(|| Job {
                id: 1,
                attempts: 1,
                spec: JobSpec {
                    name: "hung".into(),
                    cwd: Some(tmp.path().join("dead-mount").to_string_lossy().into_owned()),
                    command: vec!["/bin/echo".into()],
                    mem_mb: 64,
                    ..Default::default()
                },
                ..Default::default()
            }),
            updates: Some(updates),
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
    let mut agent = Agent::new(Wiring {
        memory_cap: idlemax_core::config::MemoryCap::Unlimited,
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
        dispatch_inputs_every: None,
        device_refresh_every: Duration::from_secs(3600),
        notify: false,
        paths: paths.clone(),
        config: Config::default(),
        worker_room: idlemax_core::dispatch::room::WorkerRoom::new(
            idlemax_core::dispatch::room::RoomPolicy::new(
                &idlemax_core::config::DispatcherConfig::default(),
                &idlemax_core::config::GuardPolicy::laptop(),
            ),
        ),
        live: std::sync::Arc::new(crate::live_config::LiveConfig::new(
            &paths,
            idlemax_core::config::Config::default(),
        )),
        workers: None,
    });
    if isolation && !hung_cwd {
        agent.online = true;
        agent.decision.action = idlemax_core::types::GuardAction::Run {
            slots: 2,
            mem_mib: 256,
        };
        let first = Job {
            id: 1,
            attempts: 1,
            spec: JobSpec {
                name: "shared-repo".into(),
                cwd: Some(tmp.path().to_string_lossy().into_owned()),
                command: vec!["/bin/echo".into(), "shared".into()],
                mem_mb: 64,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut second = first.clone();
        second.id = 2;
        let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = Arc::clone(&checks);
        agent.cwd_check = cwd::CwdCheck::new(
            Duration::from_millis(500),
            Arc::new(move |path| {
                observed.fetch_add(1, Ordering::SeqCst);
                path.is_dir()
            }),
        );
        let applied = agent.reconcile(&[], &[], &[first, second]).await;
        let started: Vec<_> = agent
            .running
            .iter()
            .filter(|job| {
                job.status == idlemax_core::types::JobStatus::Running && job.pid.is_some()
            })
            .map(|job| job.id)
            .collect();
        agent.stopping = true;
        agent.reconcile(&[], &[], &[]).await;
        let _ = stop_server.send(());
        server.await.unwrap();
        assert!(applied, "jobs sharing a healthy cwd did not both start");
        assert!(started.contains(&1) && started.contains(&2), "{started:?}");
        assert_eq!(
            checks.load(Ordering::SeqCst),
            1,
            "cwd was not checked once per batch"
        );
        return;
    }
    if isolation {
        agent.online = true;
        agent.decision.action = idlemax_core::types::GuardAction::Run {
            slots: 3,
            mem_mib: 256,
        };
        let hung = Job {
            id: 1,
            attempts: 1,
            spec: JobSpec {
                name: "hung".into(),
                cwd: Some(tmp.path().join("dead-mount").to_string_lossy().into_owned()),
                command: vec!["/bin/echo".into()],
                mem_mb: 64,
                ..Default::default()
            },
            ..Default::default()
        };
        let healthy = Job {
            id: 2,
            attempts: 1,
            spec: JobSpec {
                name: "healthy".into(),
                cwd: Some(tmp.path().to_string_lossy().into_owned()),
                command: vec!["/bin/echo".into(), "healthy".into()],
                mem_mb: 64,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut other_hung = hung.clone();
        other_hung.id = 3;
        other_hung.spec.cwd = Some(
            tmp.path()
                .join("other-dead-mount")
                .to_string_lossy()
                .into_owned(),
        );
        let held = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let release = Arc::clone(&held);
        let (entered, mut entered_rx) = mpsc::unbounded_channel();
        agent.cwd_check = cwd::CwdCheck::new(
            Duration::from_millis(500),
            Arc::new(move |path| {
                if path.ends_with("dead-mount") || path.ends_with("other-dead-mount") {
                    let _ = entered.send(());
                    let (lock, ready) = &*held;
                    drop(
                        ready
                            .wait_while(lock.lock().unwrap(), |released| !*released)
                            .unwrap(),
                    );
                    true
                } else {
                    path.is_dir()
                }
            }),
        );
        let same_path = hung.spec.cwd.clone();
        let assignments = [hung, healthy, other_hung];
        let (result, concurrent) = tokio::join!(
            agent.reconcile(&[], &[], &assignments),
            tokio::time::timeout(Duration::from_millis(250), async {
                entered_rx.recv().await.unwrap();
                entered_rx.recv().await.unwrap();
            })
        );
        let repeated = agent
            .cwd_check
            .check(crate::agent::runner::expand_cwd(same_path.as_deref()).unwrap())
            .await;
        let healthy_started = agent.running.iter().any(|job| {
            job.id == 2
                && job.status == idlemax_core::types::JobStatus::Running
                && job.pid.is_some()
        });
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        agent.stopping = true;
        agent.reconcile(&[], &[], &[]).await;
        assert!(!result);
        assert!(
            concurrent.is_ok(),
            "batch cwd checks did not begin concurrently"
        );
        assert!(
            matches!(repeated, Err(cwd::CwdError::Pending(_))),
            "{repeated:?}"
        );
        assert!(
            entered_rx.try_recv().is_err(),
            "duplicate path started another probe"
        );
        assert!(
            healthy_started,
            "a hung cwd prevented a healthy cwd from starting"
        );
        assert!(!paths.job_log_file(1).exists());
        assert!(!paths.job_log_file(3).exists());
        let _ = stop_server.send(());
        server.await.unwrap();
        return;
    }
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (entered, entered_rx) = oneshot::channel();
    let (returned, returned_rx) = oneshot::channel();
    let probe_state = Mutex::new((Some(entered), held, Some(returned)));
    if hung_cwd {
        agent.cwd_check = cwd::CwdCheck::new(
            Duration::from_millis(100),
            Arc::new(move |_| {
                let mut state = probe_state.lock().unwrap();
                if let Some(entered) = state.0.take() {
                    let _ = entered.send(());
                }
                let _ = state.1.recv();
                if let Some(returned) = state.2.take() {
                    let _ = returned.send(());
                }
                true
            }),
        );
    }
    let (control, controls) = mpsc::unbounded_channel();
    let (_reload, reloads) = mpsc::unbounded_channel();
    let (stop_agent, stopped_agent) = oneshot::channel();
    let task = tokio::spawn(agent.run(
        async {
            let _ = stopped_agent.await;
        },
        controls,
        reloads,
    ));
    if hung_cwd {
        let entered = tokio::time::timeout(Duration::from_secs(15), entered_rx).await;
        let (applied, applied_rx) = oneshot::channel();
        control
            .send(crate::agent::api::LocalControl {
                action: idlemax_core::types::ControlAction::Pause,
                applied,
            })
            .unwrap();
        let responsive = tokio::time::timeout(Duration::from_secs(2), applied_rx).await;
        let failed = tokio::time::timeout(Duration::from_secs(2), updated.recv()).await;
        let no_log = !paths.job_log_file(1).exists();
        let _ = release.send(());
        let returned = tokio::time::timeout(Duration::from_secs(2), returned_rx).await;
        let _ = stop_agent.send(());
        task.await.unwrap();
        let _ = stop_server.send(());
        server.await.unwrap();
        assert!(entered.is_ok());
        assert!(
            responsive.is_ok(),
            "the loop did not handle pause while the cwd probe was stuck"
        );
        let failed = failed.unwrap().unwrap();
        assert_eq!(failed.status, idlemax_core::types::JobStatus::Failed);
        let error = failed.error.unwrap();
        assert!(error.contains("cwd ") && error.contains("100ms"), "{error}");
        assert!(returned.is_ok());
        assert!(
            no_log && !paths.job_log_file(1).exists(),
            "late success launched the job"
        );
        return;
    }
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
