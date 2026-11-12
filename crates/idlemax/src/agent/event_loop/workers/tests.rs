//! Tests: the agent loop never waits for the worker manager: a local pause reaches a plain
//! job at once while the manager is stuck in a slow call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use idlemax_core::config::{Config, GuardPolicy};
use idlemax_core::dispatch::room::{LineState, RoomDecision};
use idlemax_core::quota::QuotaConfig;
use idlemax_core::types::{
    ControlAction, CostClass, DeviceInfo, GuardAction, Job, JobSpec, JobStatus, UnixMs,
};
use idlemax_core::worker::WorkerSpec;
use idlemax_core::Paths;
use tokio::sync::{oneshot, watch};

use super::super::{Agent, Wiring};
use super::WorkerLink;
use crate::agent::api::LocalControl;
use crate::agent::client::CoordClient;
use crate::agent::outbox::DurableOutbox;
use crate::agent::probes::Probes;
use crate::agent::runner::os::Sig;
use crate::agent::runner::Runner;
use crate::agent::worker::cmd::{Cmd, CmdError, Commands, Output};
use crate::agent::worker::host::FakeHost;
use crate::agent::worker::procs::{Member, Procs};
use crate::agent::worker::Workers;
use crate::agent::{metrics::Sampler, ring::MetricsRing};
use crate::state::now_ms;

/// A process table whose every read waits while `stuck` is set.
struct SlowProcs {
    stuck: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
}

impl SlowProcs {
    fn wait(&self) {
        self.entered.store(true, Ordering::SeqCst);
        while self.stuck.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Procs for SlowProcs {
    fn environ(&mut self, _pid: u32) -> Option<Vec<(String, String)>> {
        None
    }
    fn members(&mut self, _shell: u32, _tag: u64, _started_ms: UnixMs) -> Vec<Member> {
        Vec::new()
    }
    fn alive(&mut self, _members: &[Member], _tag: u64, _started_ms: UnixMs) -> Vec<u32> {
        self.wait();
        Vec::new()
    }
    fn rss(&mut self, _pids: &[u32]) -> u64 {
        0
    }
    fn signal(&mut self, _pids: &[u32], _sig: Sig) {}
    fn top_memory(&mut self, _n: usize, _exclude: &[u32]) -> String {
        String::new()
    }
}

/// Every command waits while `stuck` is set, then fails.
struct SlowCommands {
    stuck: Arc<AtomicBool>,
}

impl Commands for SlowCommands {
    fn run(&self, _cmd: &Cmd) -> Result<Output, CmdError> {
        while self.stuck.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(Output {
            code: Some(1),
            stdout: String::new(),
            stderr: "released".into(),
        })
    }
}

fn agent(paths: &Paths, link: WorkerLink) -> Agent {
    let mut config = Config::default();
    config.agent.command_allowlist = vec!["/bin/sleep".into(), "claude".into()];
    let probes = Probes::new(
        Sampler::new().unwrap(),
        MetricsRing::open(&paths.agent_state_dir(), 30).unwrap(),
        paths,
        QuotaConfig::default(),
        config.agent.clone(),
    );
    let (status, _status_rx) = watch::channel(Default::default());
    Agent::new(Wiring {
        memory_cap: idlemax_core::config::MemoryCap::Unlimited,
        device: DeviceInfo {
            id: "dev".into(),
            ..DeviceInfo::default()
        },
        guard: idlemax_core::guard::Guard::new(GuardPolicy::laptop()),
        client: CoordClient::new("http://127.0.0.1:9", "test").unwrap(),
        probes,
        runner: Runner::new(paths.clone(), "dev".into(), config.agent.clone()),
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
        config: config.clone(),
        worker_room: idlemax_core::dispatch::room::WorkerRoom::new(
            idlemax_core::dispatch::room::RoomPolicy::new(
                &config.dispatcher,
                &GuardPolicy::laptop(),
            ),
        ),
        live: Arc::new(crate::live_config::LiveConfig::new(paths, config.clone())),
        workers: Some((link, Vec::new())),
    })
}

fn jobs(repo: &str) -> [Job; 2] {
    let plain = Job {
        id: 1,
        attempts: 1,
        spec: JobSpec {
            name: "plain".into(),
            command: vec!["/bin/sleep".into(), "30".into()],
            mem_mb: 64,
            ..Default::default()
        },
        ..Default::default()
    };
    let worker = Job {
        id: 2,
        attempts: 1,
        spec: JobSpec {
            name: "im-app-t-1".into(),
            command: vec!["idlemax-worker".into(), "claude".into()],
            cost: CostClass::Ai,
            ai_account: Some("max".into()),
            stdin: Some("brief".into()),
            worker: Some(WorkerSpec {
                run: 1,
                repo: repo.into(),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    [plain, worker]
}

#[tokio::test]
async fn a_local_pause_reaches_a_plain_job_while_the_manager_is_stuck() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path().join("state"));
    crate::state::ensure_dirs(&paths).unwrap();
    let stuck = Arc::new(AtomicBool::new(true));
    let entered = Arc::new(AtomicBool::new(false));
    let procs: Arc<Mutex<dyn Procs>> = Arc::new(Mutex::new(SlowProcs {
        stuck: Arc::clone(&stuck),
        entered: Arc::clone(&entered),
    }));
    let commands = Arc::new(SlowCommands {
        stuck: Arc::clone(&stuck),
    });
    let workers = Workers::new(
        Arc::new(FakeHost::default()),
        commands,
        Arc::clone(&procs),
        paths.clone(),
        "dev".into(),
    );
    let allow = vec!["claude".to_string()];
    let (link, _lost) = WorkerLink::spawn(workers, procs, allow);
    let mut agent = agent(&paths, link);
    agent.online = true;
    agent.decision.action = GuardAction::Run {
        slots: 2,
        mem_mib: 1024,
    };
    agent.room = RoomDecision {
        state: LineState::Run,
        reason: "room".into(),
        open_slots: 1,
        at_ms: now_ms(),
    };
    let repo = home.path().display().to_string();
    agent.reconcile(&[], &[], &jobs(&repo)).await;
    assert!(agent.state.is_worker(2));

    let (applied, done) = oneshot::channel();
    let (_tx, mut control) = tokio::sync::mpsc::unbounded_channel();
    let pause = LocalControl {
        action: ControlAction::Pause,
        applied,
    };
    let paused = tokio::time::timeout(
        Duration::from_secs(5),
        agent.on_local_controls(pause, &mut control),
    )
    .await;
    assert!(paused.is_ok(), "the loop waited for the worker manager");
    assert!(done.await.is_ok());
    let plain = agent.running.iter().find(|r| r.id == 1).unwrap();
    assert_eq!(plain.status, JobStatus::Paused);
    for _ in 0..200 {
        if entered.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        entered.load(Ordering::SeqCst),
        "the manager never got stuck"
    );
    assert!(stuck.load(Ordering::SeqCst));

    stuck.store(false, Ordering::SeqCst);
    agent.reconcile(&[], &[ControlAction::StopAll], &[]).await;
    for _ in 0..100 {
        agent.reap().await;
        if agent.state.jobs().all(|j| j.id != 1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
