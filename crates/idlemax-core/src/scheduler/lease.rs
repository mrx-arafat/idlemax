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

/// Active jobs whose lease ran out, as they should be stored now (Queued or Parked). A
/// worker job ends `killed` instead, and its Run decides what happens next.
pub fn expire_leases(jobs: &[Job], now_ms: UnixMs) -> Vec<Job> {
    jobs.iter()
        .filter(|j| j.status.is_active() && j.lease_expires_ms.is_some_and(|t| t <= now_ms))
        .map(|j| {
            let device = j.device.as_deref().unwrap_or("unknown device");
            if j.spec.worker.is_some() {
                return Job {
                    status: JobStatus::Killed,
                    lease_expires_ms: None,
                    finished_ms: Some(now_ms),
                    last_error: Some(format!("lease expired on {device}")),
                    ..j.clone()
                };
            }
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

/// Worker jobs whose lease ran out, with a new deadline. The sweep uses it once after the
/// coordinator itself slept: the agent on the same machine slept too and could not renew, and
/// ending its workers would make every one of them read its whole session again.
pub fn renew_worker_leases(jobs: &[Job], lease_secs: u64, now_ms: UnixMs) -> Vec<Job> {
    jobs.iter()
        .filter(|j| j.spec.worker.is_some() && j.status.is_active())
        .filter(|j| j.lease_expires_ms.is_some_and(|t| t <= now_ms))
        .map(|j| Job {
            lease_expires_ms: Some(lease_deadline(lease_secs, now_ms)),
            ..j.clone()
        })
        .collect()
}
