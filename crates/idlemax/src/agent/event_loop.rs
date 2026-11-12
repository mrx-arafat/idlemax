//! The agent event loop: sample and guard, reconcile jobs with the runner, trips to the
//! coordinator, local controls, shutdown. Adapter layer around the domain `Guard`,
//! `AgentState` and the durable outbox; it decides nothing itself.
//!
//! This file owns the loop, the agent's state and shutdown. What one tick does lives in
//! the children: `sample` (guard verdict, device info), `jobs` (reconcile, reap, local
//! controls) and `coord` (heartbeat trips, replies, the online flag, the advisor feed).

mod bounded;
mod coord;
mod cwd;
mod jobs;
mod keep_awake;
#[cfg(test)]
mod refill_tests;
mod reload;
#[cfg(test)]
mod reload_tests;
mod sample;
mod workers;

pub use workers::WorkerLink;

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use idlemax_core::config::Config;
use idlemax_core::dispatch::inputs::DispatchReply;
use idlemax_core::dispatch::room::{RoomDecision, WorkerRoom};
use idlemax_core::guard::Guard;
use idlemax_core::job_run;
use idlemax_core::protocol::{LocalStatus, RunningJob};
use idlemax_core::reconcile::AgentState;
use idlemax_core::types::{DeviceInfo, GuardDecision, UnixMs};
use idlemax_core::{Paths, VERSION};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::live_config::LiveConfig;
use crate::state::now_ms;

use super::api::{LocalControl, ReloadRequest};
use super::client::CoordClient;
use super::outbox::DurableOutbox;
use super::probes::{Probes, Reading};
use super::runner::Runner;
use super::uplink::{self, Trip};

const UPDATES_PER_TRIP: usize = 50;
/// Waiting for jobs to exit on shutdown, plus one last flush: above the runner's 15 s
/// SIGKILL escalation and within launchd's default 20 s exit timeout.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(16);
const SHUTDOWN_FLUSH: Duration = Duration::from_secs(3);
/// Sample and heartbeat period while a worker runs.
const WORKER_EVERY: Duration = Duration::from_secs(5);

/// Everything the loop needs, built by `agent::run`.
pub struct Wiring {
    pub memory_cap: idlemax_core::config::MemoryCap,
    pub device: DeviceInfo,
    pub guard: Guard,
    pub client: CoordClient,
    pub probes: Probes,
    pub runner: Runner,
    pub outbox: DurableOutbox,
    pub pause_file: PathBuf,
    pub status: watch::Sender<LocalStatus>,
    pub sample_every: Duration,
    pub heartbeat_every: Duration,
    /// How often to send the advisor inputs; None sends nothing.
    pub advisor_inputs_every: Option<Duration>,
    /// How often to send the dispatch inputs; None sends nothing.
    pub dispatch_inputs_every: Option<Duration>,
    pub device_refresh_every: Duration,
    pub notify: bool,
    /// Where `config.toml` lives and the settings the agent started with, so a reload can
    /// tell what changed.
    pub paths: Paths,
    pub config: Config,
    /// The worker room rule; its numbers follow `live` on every sample.
    pub worker_room: WorkerRoom,
    /// The newest `config.toml`, for the `[dispatcher]` keys (A2).
    pub live: Arc<LiveConfig>,
    /// The worker manager and the updates of the workers it could not take back; None
    /// where no process table can be read, so no worker can start.
    pub workers: Option<(WorkerLink, Vec<idlemax_core::protocol::JobUpdate>)>,
}

pub struct Agent {
    memory_cap: idlemax_core::config::MemoryCap,
    server_profile: bool,
    memory_limit: Option<idlemax_core::types::MemoryLimit>,
    device: DeviceInfo,
    guard: Guard,
    client: CoordClient,
    probes: Arc<Mutex<Probes>>,
    runner: Arc<Mutex<Runner>>,
    cwd_check: cwd::CwdCheck,
    pause_file: PathBuf,
    status: watch::Sender<LocalStatus>,
    sample_every: Duration,
    heartbeat_every: Duration,
    advisor_inputs_every: Option<Duration>,
    dispatch_inputs_every: Option<Duration>,
    /// The switched-on projects of the last dispatch reply.
    dispatch_projects: Vec<idlemax_core::dispatch::project::Project>,
    device_refresh_every: Duration,
    notify: bool,
    paths: Paths,
    /// The settings the agent runs with; a reload compares the file against these.
    config: Config,
    state: AgentState,
    outbox: DurableOutbox,
    reading: Reading,
    decision: GuardDecision,
    worker_room: WorkerRoom,
    live: Arc<LiveConfig>,
    /// The worker room of the newest sample.
    room: RoomDecision,
    /// This device's limits for the heartbeat, and the config they were read from (A23).
    limits: Option<Vec<idlemax_core::config::Setting>>,
    limits_of: Option<Arc<Config>>,
    running: Vec<RunningJob>,
    /// Where the running jobs work, so the disk reading covers their filesystems.
    job_dirs: Vec<PathBuf>,
    online: bool,
    last_heartbeat_ms: Option<UnixMs>,
    /// Shutting down: every tick kills, nothing starts.
    stopping: bool,
    refill_due: bool,
    queued_work: bool,
    assertion: super::keep_awake::Assertion,
    keep_awake_status: String,
    workers: Option<WorkerLink>,
}

impl Agent {
    pub fn new(mut w: Wiring) -> Self {
        let memory_cap = w.memory_cap;
        let server_profile = cfg!(target_os = "linux")
            && w.config
                .agent
                .profile_or_default(idlemax_core::types::Os::Linux)
                != idlemax_core::types::DeviceProfile::Laptop;
        let mut policy = w.guard.policy().clone();
        let memory_limit = policy.apply_memory_cap(memory_cap, server_profile);
        w.guard.set_policy(policy);
        Self {
            memory_cap,
            server_profile,
            memory_limit,
            device: w.device,
            guard: w.guard,
            client: w.client,
            probes: Arc::new(Mutex::new(w.probes)),
            runner: Arc::new(Mutex::new(w.runner)),
            cwd_check: cwd::CwdCheck::new(cwd::CWD_CHECK_LIMIT, Arc::new(std::path::Path::is_dir)),
            pause_file: w.pause_file,
            status: w.status,
            sample_every: w.sample_every,
            heartbeat_every: w.heartbeat_every,
            advisor_inputs_every: w.advisor_inputs_every,
            dispatch_inputs_every: w.dispatch_inputs_every,
            dispatch_projects: Vec::new(),
            device_refresh_every: w.device_refresh_every,
            notify: w.notify,
            paths: w.paths,
            config: w.config,
            state: AgentState::default(),
            outbox: w.outbox,
            reading: Reading::default(),
            decision: GuardDecision::default(),
            worker_room: w.worker_room,
            live: w.live,
            room: RoomDecision::hold("no sample yet", now_ms()),
            limits: None,
            limits_of: None,
            running: Vec::new(),
            job_dirs: Vec::new(),
            online: false,
            last_heartbeat_ms: None,
            stopping: false,
            refill_due: false,
            queued_work: false,
            assertion: super::keep_awake::Assertion::default(),
            keep_awake_status: "off (disabled)".into(),
            workers: None,
        }
        .with_workers(w.workers)
    }
    /// Runs until `shutdown` resolves, then stops every job. Never returns early.
    pub async fn run(
        mut self,
        shutdown: impl Future<Output = ()>,
        mut control: mpsc::UnboundedReceiver<LocalControl>,
        mut reload: mpsc::UnboundedReceiver<ReloadRequest>,
    ) {
        tokio::pin!(shutdown);
        let mut sample = ticker(self.sample_every, false);
        let mut beat = ticker(self.heartbeat_every, true);
        let mut reap_every = job_run::POLL_RELAXED;
        let mut reap = ticker(reap_every, false);
        let mut exits = tokio::time::interval(Duration::from_millis(250));
        exits.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut refresh_device = ticker(self.device_refresh_every, false);
        let mut trip: Option<JoinHandle<Trip>> = None;
        let mut beat_due = false;
        let feed_on = self.advisor_inputs_every.is_some();
        let mut feed_tick = ticker(self.advisor_inputs_every.unwrap_or_default(), true);
        let mut feed: Option<JoinHandle<()>> = None;
        let mut feed_due = false;
        let dispatch_on = self.dispatch_inputs_every.is_some();
        let mut dispatch_tick = ticker(self.dispatch_inputs_every.unwrap_or_default(), true);
        let mut dispatch: Option<JoinHandle<Option<DispatchReply>>> = None;
        let mut dispatch_due = false;
        self.on_sample().await;
        let mut fast = false;
        loop {
            // While a worker runs the agent samples and beats every 5 s (A6, D11).
            if self.state.has_workers() != fast {
                fast = !fast;
                let (s, b) = match fast {
                    true => (WORKER_EVERY, WORKER_EVERY),
                    false => (self.sample_every, self.heartbeat_every),
                };
                sample = ticker(s, false);
                beat = ticker(b, false);
            }
            // The runner reaps exits and checks memory caps; faster while a job nears its cap.
            let want = job_run::poll_every(self.running.iter().map(|j| (j.rss_bytes, j.mem_mb)));
            if want != reap_every {
                reap_every = want;
                reap = ticker(want, false);
            }
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                Some(first) = control.recv() => self.on_local_controls(first, &mut control).await,
                Some(request) = reload.recv() => self.on_reload(request).await,
                Some(done) = join(&mut trip) => {
                    trip = None;
                    self.on_trip(done).await;
                }
                _ = sample.tick() => self.on_sample().await,
                _ = beat.tick() => beat_due = true,
                _ = reap.tick() => self.reap().await,
                _ = exits.tick(), if !self.running.is_empty() => self.reap_exits().await,
                Some(done) = join(&mut feed) => {
                    feed = None;
                    log_feed_end(done);
                }
                _ = feed_tick.tick(), if feed_on => feed_due = true,
                Some(done) = join(&mut dispatch) => {
                    dispatch = None;
                    self.on_dispatch_reply(done);
                }
                _ = dispatch_tick.tick(), if dispatch_on => dispatch_due = true,
                _ = refresh_device.tick() => self.on_refresh_device_info().await,
                Some(update) = workers::next_update(&mut self.workers) => self.on_worker_update(update),
            }
            if self.refill_due {
                self.on_sample().await;
                self.refill_due = false;
                beat_due = true;
            }
            if trip.is_none() {
                trip = self.next_trip(&mut beat_due);
            }
            if feed_due && feed.is_none() && self.online {
                feed_due = false;
                feed = Some(self.spawn_feed());
            }
            if dispatch_due && dispatch.is_none() && self.online {
                dispatch_due = false;
                dispatch = Some(self.spawn_dispatch_feed());
            }
            self.update_keep_awake().await;
            self.publish();
        }
        if let Some(t) = trip {
            t.abort();
        }
        if let Some(d) = dispatch {
            d.abort();
        }
        if let Some(f) = feed {
            f.abort();
        }
        self.shutdown().await;
    }

    fn publish(&self) {
        self.status.send_replace(LocalStatus {
            keep_awake: Some(self.keep_awake_status.clone()),
            device: self.device.id.clone(),
            at_ms: now_ms(),
            version: VERSION.to_string(),
            coordinator_reachable: self.online,
            last_heartbeat_ms: self.last_heartbeat_ms,
            local_paused: self.state.local_paused(),
            kill_switch: self.reading.kill_switch,
            guard: self.decision.clone(),
            metrics: Some(self.reading.metrics.clone()),
            running: self.running.clone(),
            room: Some(self.room.clone()),
        });
    }

    /// Kills every job, waits for the exits, then makes one last flush.
    async fn shutdown(&mut self) {
        info!("agent stopping: killing running jobs");
        self.stopping = true;
        self.update_keep_awake().await;
        self.reconcile(&[], &[], &[]).await;
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while self.state.jobs().next().is_some() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            self.reap().await;
            self.drain_worker_updates();
        }
        self.reap().await;
        let left = self.state.jobs().count();
        if left > 0 {
            warn!(left, "jobs still exiting at shutdown");
        }
        let pending = self.outbox.pending(UPDATES_PER_TRIP);
        let flush = uplink::trip(self.client.clone(), pending, None);
        if tokio::time::timeout(SHUTDOWN_FLUSH, flush).await.is_err() {
            warn!(
                pending = self.outbox.len(),
                "job updates not delivered before exit"
            );
        }
        self.publish();
    }
}

fn log_feed_end(done: Result<(), JoinError>) {
    if let Err(e) = done {
        error!("advisor feed task failed: {e}");
    }
}

/// Runs `f` on a blocking thread. A panic inside poisons nothing for later calls.
async fn blocking<T, R>(
    cell: &Arc<Mutex<T>>,
    f: impl FnOnce(&mut T) -> R + Send + 'static,
) -> Result<R, JoinError>
where
    T: Send + 'static,
    R: Send + 'static,
{
    let cell = Arc::clone(cell);
    tokio::task::spawn_blocking(move || {
        let mut value = cell.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut value)
    })
    .await
}

/// Resolves when the task in flight finishes; never while none is.
async fn join<T>(task: &mut Option<JoinHandle<T>>) -> Option<Result<T, JoinError>> {
    match task {
        Some(handle) => Some(handle.await),
        None => std::future::pending().await,
    }
}

fn ticker(every: Duration, immediate: bool) -> Interval {
    let every = every.max(Duration::from_secs(1));
    let start = if immediate {
        Instant::now()
    } else {
        Instant::now() + every
    };
    let mut t = tokio::time::interval_at(start, every);
    t.set_missed_tick_behavior(MissedTickBehavior::Delay);
    t
}
