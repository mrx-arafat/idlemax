//! The agent event loop: sample and guard, reconcile jobs with the runner, trips to the
//! coordinator, local controls, shutdown. Adapter layer around the domain `Guard`,
//! `AgentState` and the durable outbox; it decides nothing itself.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use idlemax_core::guard::Guard;
use idlemax_core::protocol::{AgentCommand, Heartbeat, HeartbeatReply, LocalStatus, RunningJob};
use idlemax_core::reconcile::{fail_safe_decision, AgentAction, AgentState, TickInput};
use idlemax_core::types::{ControlAction, DeviceInfo, GuardDecision, Job, UnixMs};
use idlemax_core::VERSION;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::state::now_ms;

use super::advisor_feed;
use super::client::CoordClient;
use super::outbox::DurableOutbox;
use super::probes::{Probes, Reading};
use super::runner::Runner;
use super::uplink::{self, Trip};

/// Runner reaps exits and escalates kills this often.
const REAP_EVERY: Duration = Duration::from_secs(5);
const UPDATES_PER_TRIP: usize = 50;
/// Waiting for jobs to exit on shutdown, plus one last flush: above the runner's 15 s
/// SIGKILL escalation and within launchd's default 20 s exit timeout.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(16);
const SHUTDOWN_FLUSH: Duration = Duration::from_secs(3);

/// Everything the loop needs, built by `agent::run`.
pub struct Wiring {
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
    pub device_refresh_every: Duration,
}

pub struct Agent {
    device: DeviceInfo,
    guard: Guard,
    client: CoordClient,
    probes: Arc<Mutex<Probes>>,
    runner: Arc<Mutex<Runner>>,
    pause_file: PathBuf,
    status: watch::Sender<LocalStatus>,
    sample_every: Duration,
    heartbeat_every: Duration,
    advisor_inputs_every: Option<Duration>,
    device_refresh_every: Duration,
    state: AgentState,
    outbox: DurableOutbox,
    reading: Reading,
    decision: GuardDecision,
    running: Vec<RunningJob>,
    online: bool,
    last_heartbeat_ms: Option<UnixMs>,
    /// Shutting down: every tick kills, nothing starts.
    stopping: bool,
}

impl Agent {
    pub fn new(w: Wiring) -> Self {
        Self {
            device: w.device,
            guard: w.guard,
            client: w.client,
            probes: Arc::new(Mutex::new(w.probes)),
            runner: Arc::new(Mutex::new(w.runner)),
            pause_file: w.pause_file,
            status: w.status,
            sample_every: w.sample_every,
            heartbeat_every: w.heartbeat_every,
            advisor_inputs_every: w.advisor_inputs_every,
            device_refresh_every: w.device_refresh_every,
            state: AgentState::default(),
            outbox: w.outbox,
            reading: Reading::default(),
            decision: GuardDecision::default(),
            running: Vec::new(),
            online: false,
            last_heartbeat_ms: None,
            stopping: false,
        }
    }

    /// Runs until `shutdown` resolves, then stops every job. Never returns early.
    pub async fn run(
        mut self,
        shutdown: impl Future<Output = ()>,
        mut control: mpsc::UnboundedReceiver<ControlAction>,
    ) {
        tokio::pin!(shutdown);
        let mut sample = ticker(self.sample_every, false);
        let mut beat = ticker(self.heartbeat_every, true);
        let mut reap = ticker(REAP_EVERY, false);
        let mut refresh_device = ticker(self.device_refresh_every, false);
        let mut trip: Option<JoinHandle<Trip>> = None;
        let mut beat_due = false;
        let feed_on = self.advisor_inputs_every.is_some();
        let mut feed_tick = ticker(self.advisor_inputs_every.unwrap_or_default(), true);
        let mut feed: Option<JoinHandle<()>> = None;
        let mut feed_due = false;
        self.on_sample().await;
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                Some(first) = control.recv() => self.on_local_controls(first, &mut control).await,
                Some(done) = join(&mut trip) => {
                    trip = None;
                    self.on_trip(done).await;
                }
                _ = sample.tick() => self.on_sample().await,
                _ = beat.tick() => beat_due = true,
                _ = reap.tick() => self.reap().await,
                Some(done) = join(&mut feed) => {
                    feed = None;
                    log_feed_end(done);
                }
                _ = feed_tick.tick(), if feed_on => feed_due = true,
                _ = refresh_device.tick() => self.on_refresh_device_info().await,
            }
            if trip.is_none() {
                trip = self.next_trip(&mut beat_due);
            }
            if feed_due && feed.is_none() && self.online {
                feed_due = false;
                feed = Some(self.spawn_feed());
            }
            self.publish();
        }
        if let Some(t) = trip {
            t.abort();
        }
        if let Some(f) = feed {
            f.abort();
        }
        self.shutdown().await;
    }

    async fn on_sample(&mut self) {
        self.reap().await;
        let decision = match blocking(&self.probes, Probes::read).await {
            Ok(reading) => {
                self.reading = reading;
                self.guard.evaluate(
                    &self.reading.metrics,
                    &self.running,
                    self.reading.kill_switch,
                    self.state.local_paused(),
                )
            }
            Err(e) => {
                error!("sample worker failed: {e}");
                fail_safe_decision("metrics sample failed", now_ms())
            }
        };
        if decision.action != self.decision.action {
            info!(action = ?decision.action, reasons = ?decision.reasons, "guard verdict changed");
        }
        self.decision = decision;
        self.reconcile(&[], &[], &[]).await;
    }

    async fn on_refresh_device_info(&mut self) {
        match blocking(&self.probes, Probes::refresh_device_info).await {
            Ok(device) => {
                if device.capabilities != self.device.capabilities {
                    info!(
                        old = ?self.device.capabilities,
                        new = ?device.capabilities,
                        "device capabilities refreshed"
                    );
                }
                self.device = device;
            }
            Err(e) => error!("refresh device info failed: {e}"),
        }
    }

    async fn on_local_controls(
        &mut self,
        first: ControlAction,
        control: &mut mpsc::UnboundedReceiver<ControlAction>,
    ) {
        let mut controls = vec![first];
        while let Ok(c) = control.try_recv() {
            controls.push(c);
        }
        info!(?controls, "local control");
        self.reconcile(&[], &controls, &[]).await;
    }

    async fn on_trip(&mut self, done: Result<Trip, JoinError>) {
        let trip = match done {
            Ok(t) => t,
            Err(e) => {
                error!("uplink task failed: {e}");
                return;
            }
        };
        if let Some(seq) = trip.acked {
            self.outbox.ack(seq);
        }
        if let Some(e) = trip.flush_error {
            self.set_online(false, &e.to_string());
        }
        match trip.beat {
            Some(Ok(reply)) => {
                self.set_online(true, "");
                self.last_heartbeat_ms = Some(now_ms());
                self.apply_reply(reply).await;
            }
            Some(Err(e)) => self.set_online(false, &e.to_string()),
            None => {}
        }
    }

    async fn apply_reply(&mut self, reply: HeartbeatReply) {
        if !reply.assign.is_empty() {
            // Re-check the kill switch at start time, not only once per sample.
            let present = tokio::fs::try_exists(&self.pause_file)
                .await
                .unwrap_or(true);
            self.reading.kill_switch = present;
        }
        self.reconcile(&reply.commands, &[], &reply.assign).await;
    }

    /// Plans with the domain state, runs the actions in the runner, queues the updates.
    async fn reconcile(
        &mut self,
        commands: &[AgentCommand],
        local: &[ControlAction],
        assign: &[Job],
    ) {
        let input = TickInput {
            guard: &self.decision,
            kill_switch: self.reading.kill_switch,
            online: self.online,
            commands,
            local_controls: local,
            assign,
            shutting_down: self.stopping,
        };
        let actions = self.state.plan(&input);
        if actions.is_empty() {
            return;
        }
        let outcomes = self.with_runner(move |r| {
            actions
                .into_iter()
                .map(|a| {
                    let outcome = execute(r, &a);
                    (a, outcome)
                })
                .collect::<Vec<_>>()
        });
        let Some(outcomes) = outcomes.await else {
            return;
        };
        let now = now_ms();
        for (action, outcome) in outcomes {
            log_action(&action, &outcome);
            if let Some(u) = self.state.applied(&action, outcome, &self.device.id, now) {
                self.outbox.push(u);
            }
        }
    }

    /// Reaps finished jobs into the outbox.
    async fn reap(&mut self) {
        let Some(updates) = self.with_runner(Runner::poll).await else {
            return;
        };
        for u in updates {
            let u = self.state.finished(u);
            info!(job = u.id, status = %u.status, exit_code = ?u.exit_code, error = ?u.error, "job ended");
            self.outbox.push(u);
        }
    }

    /// Runs `f` on the runner off the async threads and refreshes the running list.
    async fn with_runner<R: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut Runner) -> R + Send + 'static,
    ) -> Option<R> {
        let done = blocking(&self.runner, move |r| {
            let out = f(r);
            (out, r.running())
        });
        match done.await {
            Ok((out, running)) => {
                self.running = running;
                Some(out)
            }
            Err(e) => {
                error!("runner worker failed: {e}");
                None
            }
        }
    }

    /// A heartbeat trip when one is due, else a flush when online with updates waiting.
    fn next_trip(&mut self, beat_due: &mut bool) -> Option<JoinHandle<Trip>> {
        let heartbeat = if *beat_due {
            Some(self.heartbeat())
        } else {
            None
        };
        if heartbeat.is_none() && (!self.online || self.outbox.is_empty()) {
            return None;
        }
        *beat_due = false;
        let pending = self.outbox.pending(UPDATES_PER_TRIP);
        Some(tokio::spawn(uplink::trip(
            self.client.clone(),
            pending,
            heartbeat,
        )))
    }

    /// Sends the advisor inputs in its own task, so slow `gh` or session reads never stall the loop.
    fn spawn_feed(&self) -> JoinHandle<()> {
        let (client, device) = (self.client.clone(), self.device.id.clone());
        tokio::spawn(advisor_feed::run(client, device))
    }

    fn heartbeat(&self) -> Heartbeat {
        Heartbeat {
            device: self.device.clone(),
            metrics: self.reading.metrics.clone(),
            guard: self.decision.clone(),
            running: self.running.clone(),
            quota: self.reading.quota.clone(),
            interactive_use: self.reading.interactive_use.clone(),
            local_paused: self.reading.kill_switch || self.state.local_paused(),
        }
    }

    fn set_online(&mut self, online: bool, why: &str) {
        if online && !self.online {
            info!("coordinator reachable");
        } else if !online && self.online {
            warn!(
                error = why,
                "coordinator unreachable: no new jobs, still guarding running ones"
            );
        }
        self.online = online;
    }

    fn publish(&self) {
        self.status.send_replace(LocalStatus {
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
        });
    }

    /// Kills every job, waits for the exits, then makes one last flush.
    async fn shutdown(&mut self) {
        info!("agent stopping: killing running jobs");
        self.stopping = true;
        self.reconcile(&[], &[], &[]).await;
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while self.state.jobs().next().is_some() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            self.reap().await;
        }
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

fn execute(runner: &mut Runner, action: &AgentAction) -> Result<(), String> {
    let done = match action {
        AgentAction::Start(job) => runner.start(job),
        AgentAction::Pause(id) => runner.pause(*id),
        AgentAction::Resume(id) => runner.resume(*id),
        AgentAction::Kill { id, .. } => runner.kill(*id),
        AgentAction::Return { .. } => return Ok(()),
    };
    done.map_err(|e| e.to_string())
}

fn log_action(action: &AgentAction, outcome: &Result<(), String>) {
    let job = action.job_id();
    match (action, outcome) {
        (AgentAction::Start(j), Ok(())) => info!(job, name = %j.spec.name, "job started"),
        (AgentAction::Return { reason, .. }, _) => info!(job, %reason, "lease returned"),
        (AgentAction::Pause(_), Ok(())) => info!(job, "job paused"),
        (AgentAction::Resume(_), Ok(())) => info!(job, "job resumed"),
        (AgentAction::Kill { order, .. }, Ok(())) => {
            warn!(job, reason = %order.reason, owner_stop = order.owner_stop, "killing job");
        }
        (_, Err(e)) => warn!(job, error = %e, "runner action failed"),
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
