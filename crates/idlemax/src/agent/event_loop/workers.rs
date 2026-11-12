//! The worker manager's own thread and its link to the agent loop. The loop sends start,
//! pause, resume and kill over a channel and never waits, so a slow `herdr` or git call never
//! delays sampling, the guard or a local control; the manager ticks every 2 s while it
//! tracks a worker and sends its job updates back. Adapter layer.

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use idlemax_core::protocol::{JobUpdate, RunningJob};
use idlemax_core::types::Job;
use tokio::sync::mpsc;
use tracing::warn;

use super::Agent;
use crate::agent::worker::procs::Procs;
use crate::agent::worker::{Msg, Workers};
use crate::state::now_ms;

#[cfg(test)]
mod tests;

/// How often the manager ticks while it tracks a worker.
const TICK: Duration = Duration::from_secs(2);
/// How long it sleeps with nothing to watch.
const IDLE_WAIT: Duration = Duration::from_secs(60);

enum LinkMsg {
    Worker(Msg),
    Allowlist(Vec<String>),
}

/// What the manager last published for the loop to read.
#[derive(Default)]
struct Snapshot {
    running: Vec<RunningJob>,
    members: Vec<u32>,
}

pub struct WorkerLink {
    tx: Sender<LinkMsg>,
    pub(super) updates: mpsc::UnboundedReceiver<JobUpdate>,
    snapshot: Arc<Mutex<Snapshot>>,
    procs: Arc<Mutex<dyn Procs>>,
    /// The recovered workers the agent's tick state must take back.
    pub(super) adopted: Vec<Job>,
}

impl WorkerLink {
    /// Takes back what a previous agent left, then starts the manager's thread. Returns the
    /// link and the updates of the workers that were lost.
    pub fn spawn(
        mut workers: Workers,
        procs: Arc<Mutex<dyn Procs>>,
        allowlist: Vec<String>,
    ) -> (Self, Vec<JobUpdate>) {
        let origin = Instant::now();
        let awake = move || i64::try_from(origin.elapsed().as_millis()).unwrap_or(i64::MAX);
        workers.set_allowlist(allowlist);
        let lost = workers.recover(now_ms(), awake());
        let adopted = workers.jobs();
        let (tx, rx) = channel::<LinkMsg>();
        let (utx, urx) = mpsc::unbounded_channel();
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let shared = Arc::clone(&snapshot);
        std::thread::spawn(move || {
            let mut last_tick = Instant::now();
            loop {
                let wait = if workers.is_tracking() {
                    TICK
                } else {
                    IDLE_WAIT
                };
                let mut updates = Vec::new();
                match rx.recv_timeout(wait.saturating_sub(last_tick.elapsed())) {
                    Ok(LinkMsg::Worker(msg)) => {
                        updates.extend(workers.handle(msg, now_ms(), awake()));
                    }
                    Ok(LinkMsg::Allowlist(list)) => workers.set_allowlist(list),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if last_tick.elapsed() >= TICK {
                    last_tick = Instant::now();
                    updates.extend(workers.tick(now_ms(), awake()));
                }
                *shared.lock().unwrap_or_else(PoisonError::into_inner) = Snapshot {
                    running: workers.running(),
                    members: workers.member_pids(),
                };
                for u in updates {
                    if utx.send(u).is_err() {
                        return;
                    }
                }
            }
        });
        let link = Self {
            tx,
            updates: urx,
            snapshot,
            procs,
            adopted,
        };
        (link, lost)
    }

    /// Hands a message to the manager; it never waits for the manager.
    pub fn send(&self, msg: Msg) {
        if self.tx.send(LinkMsg::Worker(msg)).is_err() {
            warn!("the worker manager has stopped");
        }
    }

    pub fn set_allowlist(&self, list: Vec<String>) {
        let _ = self.tx.send(LinkMsg::Allowlist(list));
    }

    /// The workers as the heartbeat reports them.
    pub fn running(&self) -> Vec<RunningJob> {
        let snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        snapshot.running.clone()
    }

    /// The three biggest memory users outside every worker (D14).
    pub fn others(&self) -> String {
        let members = {
            let snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
            snapshot.members.clone()
        };
        let mut procs = self.procs.lock().unwrap_or_else(PoisonError::into_inner);
        procs.top_memory(3, &members)
    }
}

impl Agent {
    /// Takes back the recovered workers and queues the updates of the lost ones.
    pub(super) fn with_workers(mut self, workers: Option<(WorkerLink, Vec<JobUpdate>)>) -> Self {
        if let Some((link, lost)) = workers {
            let now = now_ms();
            for job in &link.adopted {
                self.state.adopt(job, now);
            }
            for update in lost {
                self.outbox.push(update);
            }
            self.workers = Some(link);
        }
        self
    }

    /// One update from the manager: through the tick state, like the runner's, then queued.
    pub(super) fn on_worker_update(&mut self, update: JobUpdate) {
        let update = self.state.finished(update);
        self.refill_due |= update.status.is_terminal();
        self.outbox.push(update);
        self.merge_worker_running();
    }

    /// Every update the manager has sent so far, without waiting.
    pub(super) fn drain_worker_updates(&mut self) {
        let mut ready = Vec::new();
        if let Some(link) = self.workers.as_mut() {
            while let Ok(update) = link.updates.try_recv() {
                ready.push(update);
            }
        }
        for update in ready {
            self.on_worker_update(update);
        }
    }

    /// The runner's jobs plus the manager's workers, as the heartbeat and the guard read them.
    pub(super) fn merge_worker_running(&mut self) {
        let Some(link) = &self.workers else {
            return;
        };
        self.running.retain(|r| !self.state.is_worker(r.id));
        self.running.extend(link.running());
    }
}

/// The manager's next update; never while there is no manager.
pub(super) async fn next_update(link: &mut Option<WorkerLink>) -> Option<JobUpdate> {
    match link {
        Some(link) => link.updates.recv().await,
        None => std::future::pending().await,
    }
}
