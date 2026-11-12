//! The coordinator side: heartbeat trips and their replies, the online flag and the advisor
//! feed task. Part of the agent event loop (adapter layer).

use idlemax_core::dispatch::inputs::DispatchReply;
use idlemax_core::dispatch::room::LineState;
use idlemax_core::protocol::{Heartbeat, HeartbeatReply};
use tokio::task::{JoinError, JoinHandle};
use tracing::{error, info, warn};

use super::{Agent, UPDATES_PER_TRIP};
use crate::agent::advisor_feed;
use crate::agent::dispatch_feed::{self, FeedContext};
use crate::agent::uplink::{self, Trip};
use crate::state::now_ms;

impl Agent {
    pub(super) async fn on_trip(&mut self, done: Result<Trip, JoinError>) {
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
        self.queued_work = reply.queued_work;
        if !reply.assign.is_empty() {
            // Re-check the kill switch at start time, not only once per sample.
            let present = tokio::fs::try_exists(&self.pause_file)
                .await
                .unwrap_or(true);
            self.reading.kill_switch = present;
        }
        self.reconcile(&reply.commands, &[], &reply.assign).await;
    }

    /// A heartbeat trip when one is due, else a flush when online with updates waiting.
    pub(super) fn next_trip(&mut self, beat_due: &mut bool) -> Option<JoinHandle<Trip>> {
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
    pub(super) fn spawn_feed(&self) -> JoinHandle<()> {
        let (client, device) = (self.client.clone(), self.device.id.clone());
        tokio::spawn(advisor_feed::run(client, device))
    }

    /// Sends the dispatch inputs in its own task; Herdr and git reads never stall the loop.
    pub(super) fn spawn_dispatch_feed(&self) -> JoinHandle<Option<DispatchReply>> {
        let ctx = FeedContext {
            device: self.device.id.clone(),
            paths: self.paths.clone(),
            projects: self.dispatch_projects.clone(),
            room_run: self.room.state == LineState::Run,
            local_paused: self.state.local_paused(),
            job_dirs: self.job_dirs.clone(),
        };
        tokio::spawn(dispatch_feed::run(self.client.clone(), ctx))
    }

    /// Keeps the reply's projects for the next run; a failed run keeps the last ones.
    pub(super) fn on_dispatch_reply(&mut self, done: Result<Option<DispatchReply>, JoinError>) {
        match done {
            Ok(Some(reply)) => self.dispatch_projects = reply.projects,
            Ok(None) => {}
            Err(e) => warn!("dispatch feed task failed: {e}"),
        }
    }

    fn heartbeat(&self) -> Heartbeat {
        Heartbeat {
            keep_awake: Some(self.keep_awake_status.clone()),
            device: self.device.clone(),
            metrics: self.reading.metrics.clone(),
            guard: self.decision.clone(),
            running: self.running.clone(),
            quota: self.reading.quota.clone(),
            interactive_use: self.reading.interactive_use.clone(),
            local_paused: self.reading.kill_switch || self.state.local_paused(),
            room: Some(self.room.clone()),
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
}
