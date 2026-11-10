//! Adapter: combines fresh agent facts and the domain keep-awake decision.

use idlemax_core::keep_awake::decide;
use idlemax_core::types::JobStatus;

use super::Agent;

impl Agent {
    pub(super) async fn update_keep_awake(&mut self) {
        let queued = self.online
            && self.last_heartbeat_ms.is_some_and(|at| {
                crate::state::now_ms().saturating_sub(at)
                    <= i64::try_from(self.heartbeat_every.as_millis().saturating_mul(2))
                        .unwrap_or(i64::MAX)
            })
            && self.queued_work;
        let decision = decide(
            self.config.agent.keep_awake,
            self.reading.metrics.on_ac_power,
            self.running.iter().any(|j| j.status == JobStatus::Running),
            queued,
            &self.decision,
            self.stopping || self.reading.kill_switch || self.state.local_paused(),
        );
        self.keep_awake_status = match self.assertion.apply(decision.hold).await {
            Ok(()) => decision.reason.to_string(),
            Err(e) => {
                tracing::warn!("keep-awake assertion failed: {e}");
                format!("not holding (OS assertion failed: {e})")
            }
        };
    }
}
