//! Domain: the retry and park policy for an attempt that ended for a reason that is not the
//! job's fault.

use crate::types::{CostClass, Job, JobStatus, UnixMs};

pub const BACKOFF_BASE_MS: i64 = 120_000;
pub const BACKOFF_MAX_MS: i64 = 3_600_000;

/// Delay before the next attempt after `attempts` leases: 2, 4, 8 ... min, capped at 1 h.
pub fn backoff_ms(attempts: u32) -> i64 {
    let exp = attempts.saturating_sub(1).min(16);
    (BACKOFF_BASE_MS << exp).min(BACKOFF_MAX_MS)
}

/// The job after an attempt ended for a reason that is not its fault (a guard kill, an agent
/// that shut down or could not start it, a lost lease): back to Queued with backoff, or
/// Parked after `effective_max_attempts` leases. AI jobs always park.
pub fn retry_or_park(job: &Job, now_ms: UnixMs, cause: &str) -> Job {
    let max = job.spec.effective_max_attempts();
    let mut next = Job {
        device: None,
        lease_expires_ms: None,
        ..job.clone()
    };
    if job.spec.cost == CostClass::Ai {
        next.status = JobStatus::Parked;
        next.last_error = Some(format!("{cause}; AI jobs are never retried"));
    } else if job.attempts >= max {
        next.status = JobStatus::Parked;
        next.last_error = Some(format!(
            "{cause}; parked after {} of {max} attempts",
            job.attempts
        ));
    } else {
        next.status = JobStatus::Queued;
        next.not_before_ms = Some(now_ms.saturating_add(backoff_ms(job.attempts)));
        next.last_error = Some(format!(
            "{cause}; attempt {} of {max} next",
            job.attempts + 1
        ));
    }
    if next.status == JobStatus::Parked {
        next.finished_ms = Some(now_ms);
    }
    next
}
