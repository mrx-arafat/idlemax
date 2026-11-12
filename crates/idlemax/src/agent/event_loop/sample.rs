//! Sampling: one guard verdict per sample tick, and the hourly device-info refresh.
//! Part of the agent event loop (adapter layer); the verdict itself comes from the domain `Guard`.
//! Both probe calls are bounded: a probe that hangs makes the verdict fail safe (pause)
//! instead of stopping the loop.

use std::sync::Arc;
use std::time::Duration;

use idlemax_core::dispatch::room::{RoomDecision, RoomPolicy};
use idlemax_core::reconcile::fail_safe_decision;
use tracing::{info, warn};

use super::bounded::blocking_within;
use super::Agent;
use crate::agent::probes::Probes;
use crate::state::now_ms;

/// Longest the loop waits for one batch of probes. Each probe command is capped at 2 s, so a
/// healthy batch is far under this; only a stuck filesystem call can reach it.
pub(super) const PROBE_LIMIT: Duration = Duration::from_secs(10);

impl Agent {
    pub(super) async fn on_sample(&mut self) {
        self.reap().await;
        let job_dirs = self.job_dirs.clone();
        let read = blocking_within(&self.probes, PROBE_LIMIT, move |p| p.read(&job_dirs));
        let mut decision = match read.await {
            Ok(reading) => {
                self.reading = reading;
                if let Some(link) = &self.workers {
                    link.set_readings(self.reading.quota.clone());
                }
                self.evaluate_room().await;
                self.guard.evaluate(
                    &self.reading.metrics,
                    &self.running,
                    self.reading.kill_switch,
                    self.state.local_paused(),
                )
            }
            Err(e) => {
                warn!("metrics sample failed: {e}");
                let why = format!("metrics sample failed: {e}");
                self.room = RoomDecision::hold(why.clone(), now_ms());
                fail_safe_decision(&why, now_ms())
            }
        };
        decision.memory_limit = self.memory_limit.clone();
        if decision.action != self.decision.action {
            info!(action = ?decision.action, reasons = ?decision.reasons, "guard verdict changed");
        }
        self.decision = decision;
        self.state.sampled(self.decision.at_ms);
        self.reconcile(&[], &[], &[]).await;
    }

    /// The worker room of the fresh sample, with the `[dispatcher]` numbers in the file now
    /// and the guard's battery floor and thermal switch (A2, A17).
    async fn evaluate_room(&mut self) {
        let live = Arc::clone(&self.live);
        let dispatcher = match tokio::task::spawn_blocking(move || live.current()).await {
            Ok(cfg) => cfg.dispatcher.clone(),
            Err(e) => {
                warn!("reading config.toml failed: {e}");
                self.config.dispatcher.clone()
            }
        };
        let policy = RoomPolicy::new(&dispatcher, self.guard.policy());
        if &policy != self.worker_room.policy() {
            self.worker_room.set_policy(policy);
        }
        let room = self.worker_room.evaluate(&self.reading.metrics);
        if room.state != self.room.state {
            info!(state = room.state.as_str(), reason = %room.reason, "worker room changed");
        }
        self.room = room;
    }

    pub(super) async fn on_refresh_device_info(&mut self) {
        match blocking_within(&self.probes, PROBE_LIMIT, Probes::refresh_device_info).await {
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
            Err(e) => warn!("refresh device info failed: {e}"),
        }
    }
}
