//! Domain: lease deadlines, and what happens to a job whose lease ran out.

use super::retry::retry_or_park;
use crate::types::{Job, UnixMs};

/// Slack on top of max runtime so the runner's timeout fires before the lease ends.
pub const LEASE_GRACE_MS: i64 = 120_000;

pub fn lease_deadline(job: &Job, now_ms: UnixMs) -> UnixMs {
    let runtime_ms =
        i64::try_from(job.spec.max_runtime_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
    now_ms
        .saturating_add(runtime_ms)
        .saturating_add(LEASE_GRACE_MS)
}

/// Active jobs whose lease ran out, as they should be stored now (Queued or Parked).
pub fn expire_leases(jobs: &[Job], now_ms: UnixMs) -> Vec<Job> {
    jobs.iter()
        .filter(|j| j.status.is_active() && j.lease_expires_ms.is_some_and(|t| t <= now_ms))
        .map(|j| {
            let device = j.device.as_deref().unwrap_or("unknown device");
            retry_or_park(j, now_ms, &format!("lease expired on {device}"))
        })
        .collect()
}
