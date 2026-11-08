//! Domain: lease deadlines, and what happens to a job whose lease ran out.

use super::retry::retry_or_park;
use crate::types::{Job, JobStatus, UnixMs};

/// `lease_secs` from now, renewed by every heartbeat that reports the job. No cap at
/// `started + max_runtime`: runtime counts awake time and the agent enforces it, so a laptop
/// that slept mid-job is past that wall-clock point while the job still has time left.
pub fn lease_deadline(lease_secs: u64, now_ms: UnixMs) -> UnixMs {
    let lease_ms = i64::try_from(lease_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
    now_ms.saturating_add(lease_ms)
}

/// Active jobs whose lease ran out, as they should be stored now (Queued or Parked).
pub fn expire_leases(jobs: &[Job], now_ms: UnixMs) -> Vec<Job> {
    jobs.iter()
        .filter(|j| j.status.is_active() && j.lease_expires_ms.is_some_and(|t| t <= now_ms))
        .map(|j| {
            let device = j.device.as_deref().unwrap_or("unknown device");
            // Never started: back to the queue, not parked. The attempt number stays so a late
            // result from this lease still matches it.
            if j.status == JobStatus::Leased {
                Job {
                    status: JobStatus::Queued,
                    device: None,
                    lease_expires_ms: None,
                    not_before_ms: None,
                    last_error: Some(format!("lease expired on {device} before starting")),
                    ..j.clone()
                }
            } else {
                retry_or_park(j, now_ms, &format!("lease expired on {device}"))
            }
        })
        .collect()
}
