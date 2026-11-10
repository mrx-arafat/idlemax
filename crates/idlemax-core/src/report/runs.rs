//! Domain: the attempts a job made, rebuilt from its lifecycle events (pure).
//!
//! A job row keeps the start and finish of its last attempt only: leasing a retried job again
//! clears them. The events the coordinator logs for each change of status, on its own clock,
//! keep every attempt.

use std::collections::BTreeMap;

use crate::types::{DeviceId, JobId, UnixMs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEventKind {
    /// The job started running.
    Started,
    /// The attempt ended: the job finished, was cancelled, parked or put back in the queue,
    /// or its lease expired.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEvent {
    pub job: JobId,
    pub device: Option<DeviceId>,
    pub at_ms: UnixMs,
    pub kind: JobEventKind,
}

/// One attempt. `to_ms` is None while the attempt has no end event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRun {
    pub job: JobId,
    pub device: Option<DeviceId>,
    pub from_ms: UnixMs,
    pub to_ms: Option<UnixMs>,
}

/// The attempts in `events`, which hold each job's events in the order they happened (jobs
/// may interleave). A start with no end before the next start ends at that next start.
pub fn runs_from_events(events: &[JobEvent]) -> Vec<JobRun> {
    let mut open: BTreeMap<JobId, JobRun> = BTreeMap::new();
    let mut done = Vec::new();
    for e in events {
        match e.kind {
            JobEventKind::Started => {
                let run = JobRun {
                    job: e.job,
                    device: e.device.clone(),
                    from_ms: e.at_ms,
                    to_ms: None,
                };
                if let Some(mut lost) = open.insert(e.job, run) {
                    lost.to_ms = Some(e.at_ms.max(lost.from_ms));
                    done.push(lost);
                }
            }
            JobEventKind::Ended => {
                if let Some(mut run) = open.remove(&e.job) {
                    run.to_ms = Some(e.at_ms.max(run.from_ms));
                    done.push(run);
                }
            }
        }
    }
    done.extend(open.into_values());
    done.sort_by_key(|r| (r.from_ms, r.job));
    done
}
