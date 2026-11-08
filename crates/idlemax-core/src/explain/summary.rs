//! Domain: the one-reason version of an explanation, for lists such as the dashboard's job
//! table. Pure: it only reads an `Explanation`.

use serde::{Deserialize, Serialize};

use super::{DeviceExplanation, ExplainState, Explanation, Reason};
use crate::types::{DeviceId, JobId, JobStatus};

/// The reason worth showing next to a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    /// The device the reason is about; None when it holds on every device.
    pub device: Option<DeviceId>,
    pub reason: Reason,
}

/// One row of GET /v1/why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhySummary {
    pub job: JobId,
    pub status: JobStatus,
    pub state: ExplainState,
    /// None when nothing holds the job back (it is ready, running or finished).
    pub headline: Option<Headline>,
    /// Other reasons the full explanation lists: those on every device plus those of the
    /// headline's device.
    pub more: usize,
}

/// What holds on every device comes first. Otherwise the headline is the first reason of the
/// device closest to running: one that only waits (no setting to change), then an online one,
/// then the one with the fewest reasons.
pub fn summarize(e: &Explanation) -> WhySummary {
    let best = best_device(e);
    let first = e
        .reasons
        .first()
        .map(|r| (None, r))
        .or_else(|| best.and_then(|d| d.reasons.first().map(|r| (Some(d), r))));
    let total = e.reasons.len() + best.map_or(0, |d| d.reasons.len());
    WhySummary {
        job: e.job,
        status: e.status,
        state: e.state,
        more: total.saturating_sub(usize::from(first.is_some())),
        headline: first.map(|(device, reason)| Headline {
            device: device.map(|d| d.device.clone()),
            reason: reason.clone(),
        }),
    }
}

fn best_device(e: &Explanation) -> Option<&DeviceExplanation> {
    if e.state == ExplainState::Ready {
        return None;
    }
    e.devices
        .iter()
        .filter(|d| !d.reasons.is_empty())
        .min_by_key(|d| {
            let permanent = d.reasons.iter().any(|r| r.kind.is_permanent());
            (permanent, !d.online, d.reasons.len())
        })
}
