//! Sampling: one guard verdict per sample tick, and the hourly device-info refresh.
//! Part of the agent event loop (adapter layer); the verdict itself comes from the domain `Guard`.

use idlemax_core::reconcile::fail_safe_decision;
use tracing::{error, info};

use super::{blocking, Agent};
use crate::agent::probes::Probes;
use crate::state::now_ms;

impl Agent {
    pub(super) async fn on_sample(&mut self) {
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

    pub(super) async fn on_refresh_device_info(&mut self) {
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
}
