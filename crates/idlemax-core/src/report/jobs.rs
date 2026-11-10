//! Domain: what job rows and attempts say about a period, and when jobs ran or waited (pure).
//!
//! Times of an attempt come from the coordinator's events when the job has any. The start
//! and finish a job row keeps come from the agent's clock, which can run ahead or behind, so
//! every time is clamped to the span between the job's creation and now.

use std::collections::BTreeMap;

use super::{JobFact, JobRun, JobSummary, PauseSpan, Window};
use crate::control::in_scope;
use crate::types::{DeviceId, JobId, JobStatus, UnixMs};

/// A half-open span of time, `[from, to)`.
pub(super) type Interval = (UnixMs, UnixMs);

/// A job gets this long to start before its wait counts as work that did not start: one
/// heartbeat to learn of it and another to lease it.
const START_GRACE_MS: i64 = 120_000;

/// One attempt of a job.
pub(super) struct Attempt {
    pub device: Option<DeviceId>,
    pub span: Interval,
}

/// A job with the times it ran and the times it waited.
pub(super) struct JobTimes<'a> {
    pub job: &'a JobFact,
    pub attempts: Vec<Attempt>,
    /// Times it waited in the queue past the start grace, whichever device could have run it.
    pub waits: Vec<Interval>,
}

pub(super) fn times<'a>(
    jobs: &'a [JobFact],
    runs: &[JobRun],
    pauses: &[PauseSpan],
    now: UnixMs,
) -> Vec<JobTimes<'a>> {
    let mut by_job: BTreeMap<JobId, Vec<&JobRun>> = BTreeMap::new();
    for run in runs {
        by_job.entry(run.job).or_default().push(run);
    }
    jobs.iter()
        .map(|job| {
            let own = by_job.get(&job.id).map_or(&[][..], Vec::as_slice);
            let (attempts, start_trusted) = attempts(job, own, now);
            let waits = waits(job, &attempts, start_trusted, now);
            let waits = subtract(waits, &held_by_pause(job, pauses, now));
            JobTimes {
                job,
                attempts,
                waits,
            }
        })
        .collect()
}

/// Every attempt the job made, and whether the first one's start can be trusted. Events give
/// them all; a job with none falls back to the start and finish its row keeps, which cover
/// the last attempt (a re-queued job has no usable start: it belongs to an old attempt).
fn attempts(job: &JobFact, runs: &[&JobRun], now: UnixMs) -> (Vec<Attempt>, bool) {
    let latest = now.max(job.created_ms);
    let clamp = |t: UnixMs| t.clamp(job.created_ms, latest);
    if !runs.is_empty() {
        let list = runs
            .iter()
            .map(|r| {
                let from = clamp(r.from_ms);
                let to = match r.to_ms {
                    Some(t) => clamp(t),
                    None if job.status.is_active() => latest,
                    None => clamp(job.finished_ms.unwrap_or(r.from_ms)),
                };
                Attempt {
                    device: r.device.clone().or_else(|| job.device.clone()),
                    span: (from, to.max(from)),
                }
            })
            .collect();
        return (list, true);
    }
    let Some(start) = job.started_ms else {
        return (Vec::new(), true);
    };
    let to = match (job.finished_ms, job.status) {
        (Some(finished), _) => clamp(finished),
        (None, JobStatus::Leased | JobStatus::Running | JobStatus::Paused) => latest,
        (None, _) => return (Vec::new(), true),
    };
    let from = clamp(start);
    // A start outside what the coordinator saw says only that the job ran, not when.
    let trusted = (job.created_ms..=latest).contains(&start);
    let attempt = Attempt {
        device: job.device.clone(),
        span: (from, to.max(from)),
    };
    (vec![attempt], trusted)
}

/// When the job waited past the start grace: before its first attempt, and, when it is back
/// in the queue, after its last one. The waits between retries are not counted (the retry
/// backoff is not the machine's fault). Parked jobs wait for a human, not for a machine.
fn waits(job: &JobFact, attempts: &[Attempt], start_trusted: bool, now: UnixMs) -> Vec<Interval> {
    let eligible = job.created_ms.max(job.not_before_ms.unwrap_or(0));
    let mut out = Vec::new();
    match (job.status, attempts.iter().map(|a| a.span.0).min()) {
        (JobStatus::Parked, _) => {}
        (_, Some(first)) if start_trusted => out.push((job.created_ms + START_GRACE_MS, first)),
        (_, Some(_)) => {}
        (JobStatus::Queued, None) => out.push((eligible + START_GRACE_MS, now)),
        (_, None) => {
            // Cancelled while it waited.
            if let Some(end) = job.finished_ms {
                out.push((job.created_ms + START_GRACE_MS, end.min(now)));
            }
        }
    }
    if let (JobStatus::Queued, Some(last)) = (job.status, attempts.iter().map(|a| a.span.1).max()) {
        out.push((eligible.max(last) + START_GRACE_MS, now));
    }
    out.retain(|(from, to)| to > from);
    out
}

/// When a pause covered the job itself (all jobs, its AI account, or the job): the scheduler's
/// own `in_scope` decides, and a held job is not waiting for room. A device pause is not here:
/// it holds the device, which the timeline counts.
fn held_by_pause(job: &JobFact, pauses: &[PauseSpan], now: UnixMs) -> Vec<Interval> {
    let as_job = job.as_job();
    let held = pauses
        .iter()
        .filter(|p| in_scope(&p.scope, &as_job))
        .map(|p| (p.from_ms, p.to_ms.unwrap_or(now)))
        .collect();
    merge(held)
}

/// `spans` with the time inside `holes` (merged, sorted) cut out.
fn subtract(spans: Vec<Interval>, holes: &[Interval]) -> Vec<Interval> {
    let mut out = Vec::new();
    for (mut from, to) in spans {
        for &(hole_from, hole_to) in holes {
            if hole_to <= from || hole_from >= to {
                continue;
            }
            if hole_from > from {
                out.push((from, hole_from));
            }
            from = from.max(hole_to);
        }
        if from < to {
            out.push((from, to));
        }
    }
    out
}

/// Sorted, non-overlapping intervals covering the same time.
pub(super) fn merge(mut intervals: Vec<Interval>) -> Vec<Interval> {
    intervals.retain(|(from, to)| to > from);
    intervals.sort_unstable();
    let mut out: Vec<Interval> = Vec::with_capacity(intervals.len());
    for (from, to) in intervals {
        match out.last_mut() {
            Some(last) if from <= last.1 => last.1 = last.1.max(to),
            _ => out.push((from, to)),
        }
    }
    out
}

fn within(window: &Window, at: UnixMs) -> bool {
    at >= window.since_ms && at <= window.until_ms
}

fn overlap(span: Interval, window: &Window) -> i64 {
    let (from, to) = (span.0.max(window.since_ms), span.1.min(window.until_ms));
    (to - from).max(0)
}

pub(super) fn summarize(times: &[JobTimes<'_>], window: &Window) -> JobSummary {
    let mut sum = JobSummary::default();
    for t in times {
        let ran = t
            .attempts
            .iter()
            .any(|a| within(window, a.span.0) || overlap(a.span, window) > 0);
        sum.ran += u64::from(ran);
        sum.job_ms += t
            .attempts
            .iter()
            .map(|a| overlap(a.span, window))
            .sum::<i64>();
        // How it ended, on the coordinator's clock when it logged that, else the agent's.
        let finished = t
            .job
            .outcome_at_ms
            .or(t.job.finished_ms)
            .map(|f| f.clamp(t.job.created_ms, window.until_ms.max(t.job.created_ms)));
        if finished.is_some_and(|at| within(window, at)) {
            match t.job.status {
                JobStatus::Succeeded => sum.succeeded += 1,
                JobStatus::Failed => sum.failed += 1,
                JobStatus::Killed => sum.killed += 1,
                JobStatus::Cancelled => sum.cancelled += 1,
                _ => {}
            }
        }
    }
    sum
}
