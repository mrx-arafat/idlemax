//! Domain: the job after an agent's update, or None when the update must be ignored.

use super::retry::retry_or_park;
use crate::protocol::JobUpdate;
use crate::types::{Job, JobStatus, UnixMs};

/// The job after an agent's update, or None when the update must be ignored (job already
/// terminal, or sent by a device that does not hold the lease). Coordinator stops and cancels
/// are made terminal when requested, so their later killed update is ignored.
/// A job's own result is final: a non-zero exit is `failed`, and a kill for its own limits
/// (memory cap, max runtime) or by the owner is `killed`. Only an update the agent marks
/// `retryable` (guard kill, agent shutdown, start failure) goes back in the queue.
/// A result for an attempt whose lease expired while its agent was away still counts when
/// the job has not been leased since (see `is_late_result`).
pub fn apply_update(job: &Job, update: &JobUpdate, now_ms: UnixMs) -> Option<Job> {
    let late = is_late_result(job, update);
    let holds_lease = job.device.as_deref() == Some(update.device.as_str());
    if !late && (job.status.is_terminal() || !holds_lease) {
        return None;
    }
    let mut next = job.clone();
    if late {
        // It ran on the reporting device, so its output lives there.
        next.device = Some(update.device.clone());
        next.not_before_ms = None;
    }
    next.exit_code = update.exit_code.or(job.exit_code);
    next.output_path = update
        .output_path
        .clone()
        .or_else(|| job.output_path.clone());
    match update.status {
        JobStatus::Leased => return None,
        JobStatus::Running | JobStatus::Paused => {
            next.status = update.status;
            next.started_ms = job.started_ms.or(Some(update.at_ms));
        }
        // Lease returned unstarted: the lease does not count as an attempt.
        JobStatus::Queued => {
            next = Job {
                status: JobStatus::Queued,
                device: None,
                lease_expires_ms: None,
                ..next
            };
            next.attempts = job.attempts.saturating_sub(1);
        }
        // Not the job's fault: run it again later, up to max_attempts.
        JobStatus::Failed | JobStatus::Killed if update.retryable && !update.owner_stop => {
            let cause = update
                .error
                .clone()
                .unwrap_or_else(|| format!("job {}", update.status));
            return Some(retry_or_park(&next, now_ms, &cause));
        }
        _ => finish(&mut next, update),
    }
    Some(next)
}

/// A terminal update for an attempt whose lease expired while its agent was away. Expiry
/// put the job back in the queue, or parked it, with `attempts` unchanged and no device; a
/// result that names that same attempt belongs to a run nothing has replaced. A job leased
/// again has more attempts or a device, and an old agent sends no attempt (0), so neither
/// matches and they stay ignored.
fn is_late_result(job: &Job, update: &JobUpdate) -> bool {
    matches!(job.status, JobStatus::Queued | JobStatus::Parked)
        && job.device.is_none()
        && update.attempt != 0
        && update.attempt == job.attempts
        && matches!(
            update.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Killed
        )
}

/// Terminal state reported by the agent: no retry, no park.
fn finish(next: &mut Job, update: &JobUpdate) {
    next.status = update.status;
    next.lease_expires_ms = None;
    next.finished_ms = Some(update.at_ms);
    next.last_error = update.error.clone();
}
