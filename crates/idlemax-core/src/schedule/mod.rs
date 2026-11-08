//! Domain: recurring job schedules and the pure decision of when one is due.
//! A schedule holds a `JobSpec` and an interval; the coordinator turns "due" into a queued job.

mod every;
#[cfg(test)]
mod tests;

pub use every::{Every, EveryError};

use serde::{Deserialize, Serialize};

use crate::types::{Job, JobId, JobSpec, JobStatus, UnixMs};

pub type ScheduleId = i64;

/// A job that repeats. At most one job of a schedule is queued or running at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: ScheduleId,
    pub spec: JobSpec,
    pub every: Every,
    #[serde(default)]
    pub paused: bool,
    pub created_ms: UnixMs,
    /// When the coordinator last queued (or tried to queue) a job for this schedule.
    pub last_fired_ms: Option<UnixMs>,
    /// The job queued by the last successful fire.
    pub last_job: Option<JobId>,
}

/// Body of `POST /v1/schedules`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewSchedule {
    pub spec: JobSpec,
    pub every: Every,
}

/// What the coordinator does with a schedule right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// Queue a new job.
    Fire,
    /// The interval has not passed; the next job is due at `until_ms`.
    Wait {
        until_ms: UnixMs,
    },
    /// The previous job is still queued, leased, running or paused.
    Running {
        job: JobId,
        status: JobStatus,
    },
    Paused,
}

/// A schedule with its current decision, as `GET /v1/schedules` returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: Schedule,
    pub decision: Decision,
}

/// Decides what to do with `schedule`, given the job its last fire queued (if any).
///
/// A job is queued only when the previous one has ended and a whole interval has passed
/// since it ended (since the fire, if it never ran). After a long outage that is one job,
/// not one per missed period, and a job that waited days for an idle machine is not
/// followed at once by the next.
pub fn decide(schedule: &Schedule, last_job: Option<&Job>, now_ms: UnixMs) -> Decision {
    if let Some(job) = last_job.filter(|j| !j.status.is_terminal()) {
        return Decision::Running {
            job: job.id,
            status: job.status,
        };
    }
    if schedule.paused {
        return Decision::Paused;
    }
    let Some(fired_ms) = schedule.last_fired_ms else {
        return Decision::Fire;
    };
    let ended_ms = last_job.and_then(|j| j.finished_ms).unwrap_or(fired_ms);
    // A clock set back must not push the next run out to the old future.
    let anchor_ms = fired_ms.max(ended_ms).min(now_ms);
    let due_ms = anchor_ms.saturating_add(schedule.every.ms());
    if now_ms >= due_ms {
        Decision::Fire
    } else {
        Decision::Wait { until_ms: due_ms }
    }
}
