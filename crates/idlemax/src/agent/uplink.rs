//! One trip to the coordinator, run as its own task so a slow or unreachable coordinator
//! never stalls the agent loop: flush pending job updates in order, then heartbeat.
//! Adapter layer.

use idlemax_core::protocol::{Heartbeat, HeartbeatReply, JobUpdate};

use super::client::{ClientError, CoordClient};

#[derive(Debug, Default)]
pub struct Trip {
    /// Highest update seq the coordinator took or rejected for good.
    pub acked: Option<u64>,
    /// Transient failure that stopped the flush; the heartbeat was skipped.
    pub flush_error: Option<ClientError>,
    /// Heartbeat outcome, when one was sent.
    pub beat: Option<Result<HeartbeatReply, ClientError>>,
}

pub async fn trip(
    client: CoordClient,
    updates: Vec<JobUpdate>,
    heartbeat: Option<Heartbeat>,
) -> Trip {
    let mut out = Trip::default();
    for update in &updates {
        match client.job_update(update).await {
            Ok(acked) => out.acked = Some(acked),
            Err(e) if e.is_rejected() => {
                tracing::warn!(
                    job = update.id,
                    "coordinator rejected job update, dropped: {e}"
                );
                out.acked = Some(update.seq);
            }
            Err(e) => {
                out.flush_error = Some(e);
                return out;
            }
        }
    }
    if let Some(hb) = heartbeat {
        out.beat = Some(client.heartbeat(&hb).await);
    }
    out
}
