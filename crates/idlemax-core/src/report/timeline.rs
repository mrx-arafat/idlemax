//! Domain: walks each device's guard history, job runs and queue wait side by side, slice by
//! slice, and totals where the time went (pure).

use std::collections::BTreeMap;

use super::jobs::{merge, Interval, JobTimes};
use super::{
    GuardSpan, GuardState, GuardSummary, HeldBy, HoldReason, IdleCause, IdleSummary, Inputs,
    JobFact, PauseSpan, Stretch, Window,
};
use crate::control::may_assign;
use crate::scheduler::device_blockers;
use crate::types::{ControlScope, CostClass, DeviceInfo, JobId, JobStatus, UnixMs};

/// Queued jobs no device can take that the report names.
pub(super) const MAX_STUCK: usize = 5;

/// What a device's time was at one moment.
enum Kind {
    /// No heartbeat: asleep, or the agent or the coordinator was down.
    Offline,
    Guard(GuardState),
}

struct Segment {
    from: UnixMs,
    to: UnixMs,
    kind: Kind,
}

/// The time each device spent, over the window, with the jobs' runs and waits laid on it.
pub(super) fn analyze(
    window: &Window,
    input: &Inputs<'_>,
    times: &[JobTimes<'_>],
) -> (GuardSummary, IdleSummary) {
    let now = window.until_ms;
    let infos: BTreeMap<&str, &DeviceInfo> =
        input.devices.iter().map(|d| (d.id.as_str(), d)).collect();
    let mut devices: BTreeMap<&str, Vec<&GuardSpan>> = BTreeMap::new();
    for span in input.spans {
        devices.entry(span.device.as_str()).or_default().push(span);
    }
    let mut total = Tally::default();
    for (device, mut spans) in devices {
        spans.sort_by_key(|s| s.start_ms);
        let segments = segments(&spans, now, input.offline_after_ms);
        let ran = times
            .iter()
            .flat_map(|t| &t.attempts)
            .filter(|a| a.device.as_deref() == Some(device))
            .map(|a| a.span)
            .collect();
        let (work, quota) = waiting_for(times, infos.get(device).copied());
        let lists = Lists {
            busy: merge(ran),
            work,
            quota,
        };
        total.absorb(walk(window, &segments, &lists, (device, input.pauses)));
    }
    let (guard, mut idle) = total.finish();
    idle.stuck_jobs = stuck(times, input.devices);
    (guard, idle)
}

/// The jobs' times laid over one device's time.
struct Lists {
    /// A job ran on the device.
    busy: Vec<Interval>,
    /// Work the device could take waited.
    work: Vec<Interval>,
    /// AI work the device could take waited.
    quota: Vec<Interval>,
}

/// Whether `device` could take `job` as it is configured: the scheduler's own check of the
/// job's device list, required tools, cost class, command allowlist and AI account. A device
/// we know nothing about is taken to be able to.
fn can_take(device: Option<&DeviceInfo>, job: &JobFact) -> bool {
    device.is_none_or(|d| device_blockers(d, &job.as_job()).is_empty())
}

/// What waited on this device, split into work and AI work, as merged spans of time.
fn waiting_for(
    times: &[JobTimes<'_>],
    device: Option<&DeviceInfo>,
) -> (Vec<Interval>, Vec<Interval>) {
    let (mut work, mut quota) = (Vec::new(), Vec::new());
    for t in times.iter().filter(|t| can_take(device, t.job)) {
        let list = if t.job.cost == CostClass::Ai {
            &mut quota
        } else {
            &mut work
        };
        list.extend(t.waits.iter().copied());
    }
    (merge(work), merge(quota))
}

/// Queued jobs that have waited past the start grace and that no known device could take.
fn stuck(times: &[JobTimes<'_>], devices: &[DeviceInfo]) -> Vec<JobId> {
    if devices.is_empty() {
        return Vec::new();
    }
    let mut ids: Vec<JobId> = times
        .iter()
        .filter(|t| t.job.status == JobStatus::Queued && !t.waits.is_empty())
        .filter(|t| devices.iter().all(|d| !can_take(Some(d), t.job)))
        .map(|t| t.job.id)
        .collect();
    ids.sort_unstable();
    ids.truncate(MAX_STUCK);
    ids
}

/// A device's history as consecutive segments. A verdict stands until the next one, unless
/// the device was silent for longer than `offline_after`: that gap is offline time.
fn segments(spans: &[&GuardSpan], now: UnixMs, offline_after: i64) -> Vec<Segment> {
    let mut out = Vec::with_capacity(spans.len() * 2);
    for (i, span) in spans.iter().enumerate() {
        let until = spans.get(i + 1).map_or(now, |next| next.start_ms);
        let to = if until - span.end_ms <= offline_after {
            until
        } else {
            span.end_ms
        };
        let to = to.max(span.start_ms);
        out.push(Segment {
            from: span.start_ms,
            to,
            kind: Kind::Guard(span.state.clone()),
        });
        if until > to {
            out.push(Segment {
                from: to,
                to: until,
                kind: Kind::Offline,
            });
        }
    }
    out
}

fn covers(merged: &[Interval], at: UnixMs) -> bool {
    let after = merged.partition_point(|(from, _)| *from <= at);
    after > 0 && at < merged[after - 1].1
}

fn segment_at(segments: &[Segment], at: UnixMs) -> Option<&Segment> {
    let after = segments.partition_point(|s| s.from <= at);
    segments[..after].last().filter(|s| at < s.to)
}

/// One device over the window: every slice between two change points is one thing.
fn walk(
    window: &Window,
    segments: &[Segment],
    lists: &Lists,
    (device, pauses): (&str, &[PauseSpan]),
) -> Tally {
    let mut tally = Tally::default();
    let Some(first) = segments.first() else {
        return tally;
    };
    let (lo, hi) = (window.since_ms.max(first.from), window.until_ms);
    if hi <= lo {
        return tally;
    }
    tally.tracking_since = Some(first.from);
    let mut cuts = vec![lo, hi];
    cuts.extend(segments.iter().flat_map(|s| [s.from, s.to]));
    let spans = lists.busy.iter().chain(&lists.work).chain(&lists.quota);
    cuts.extend(spans.flat_map(|(a, b)| [*a, *b]));
    cuts.extend(
        pauses
            .iter()
            .flat_map(|p| [p.from_ms, p.to_ms.unwrap_or(hi)]),
    );
    cuts.retain(|t| (lo..=hi).contains(t));
    cuts.sort_unstable();
    cuts.dedup();
    let mut run = Run::default();
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        tally.tracked_ms += to - from;
        let state = match segment_at(segments, from).map(|s| &s.kind) {
            Some(Kind::Guard(state)) => state,
            _ => {
                tally.offline_ms += to - from;
                tally.close(&mut run);
                continue;
            }
        };
        // A coordinator pause holds the device whatever its guard says; the scheduler's own
        // `may_assign` decides which pauses do.
        let paused = !may_assign(&paused_at(pauses, from), device, false);
        let with_pause;
        let state = if paused {
            with_pause = state.with_reason(HoldReason::Paused);
            &with_pause
        } else {
            state
        };
        tally.count_held(state, to - from);
        if covers(&lists.busy, from) {
            tally.used_ms += to - from;
            tally.close(&mut run);
        } else {
            let waiting = (covers(&lists.work, from), covers(&lists.quota, from));
            let causes = idle_causes(state, waiting);
            for cause in &causes {
                tally.count_idle(*cause, (from, to));
            }
            run.add(from, to, &causes);
        }
    }
    tally.close(&mut run);
    tally
}

/// The scopes paused at `at`.
fn paused_at(pauses: &[PauseSpan], at: UnixMs) -> Vec<ControlScope> {
    pauses
        .iter()
        .filter(|p| p.from_ms <= at && at < p.to_ms.unwrap_or(UnixMs::MAX))
        .map(|p| p.scope.clone())
        .collect()
}

/// What kept an idle device from working: each reason the guard gave, or the work and AI work
/// that waited anyway, or no work.
fn idle_causes(state: &GuardState, (work, quota): (bool, bool)) -> Vec<IdleCause> {
    if state.held {
        if state.reasons.is_empty() {
            return vec![IdleCause::Held(HoldReason::Other)];
        }
        state.reasons.iter().map(|r| IdleCause::Held(*r)).collect()
    } else {
        let mut causes = Vec::new();
        if work {
            causes.push(IdleCause::WorkWaiting);
        }
        if quota {
            causes.push(IdleCause::WaitingOnQuota);
        }
        if causes.is_empty() {
            causes.push(IdleCause::QueueEmpty);
        }
        causes
    }
}

/// An unbroken stretch of idle slices and how long each cause was present within it (a
/// slice with two reasons counts for both).
#[derive(Default)]
struct Run {
    from: UnixMs,
    to: UnixMs,
    ms: BTreeMap<IdleCause, i64>,
}

impl Run {
    fn add(&mut self, from: UnixMs, to: UnixMs, causes: &[IdleCause]) {
        if self.ms.is_empty() {
            self.from = from;
        }
        self.to = to;
        for cause in causes {
            *self.ms.entry(*cause).or_default() += to - from;
        }
    }

    /// The cause that lasted longest; on a tie the guard's reasons come before the others.
    fn dominant(&self) -> Option<IdleCause> {
        let mut best: Option<(IdleCause, i64)> = None;
        for (cause, ms) in &self.ms {
            if best.is_none_or(|(_, most)| *ms > most) {
                best = Some((*cause, *ms));
            }
        }
        best.map(|(cause, _)| cause)
    }
}

#[derive(Default)]
struct Tally {
    tracked_ms: i64,
    tracking_since: Option<UnixMs>,
    held_ms: i64,
    held_by: BTreeMap<HoldReason, i64>,
    offline_ms: i64,
    used_ms: i64,
    queue_empty_ms: i64,
    /// Slices with work or AI work waiting, merged at the end so a job that waited while two
    /// devices were idle counts once.
    work_slices: Vec<Interval>,
    quota_slices: Vec<Interval>,
    longest: Option<Stretch>,
}

impl Tally {
    fn count_held(&mut self, state: &GuardState, ms: i64) {
        if !state.held {
            return;
        }
        self.held_ms += ms;
        for reason in &state.reasons {
            *self.held_by.entry(*reason).or_default() += ms;
        }
    }

    fn count_idle(&mut self, cause: IdleCause, slice: Interval) {
        match cause {
            IdleCause::WorkWaiting => self.work_slices.push(slice),
            IdleCause::WaitingOnQuota => self.quota_slices.push(slice),
            IdleCause::QueueEmpty => self.queue_empty_ms += slice.1 - slice.0,
            IdleCause::Held(_) => {}
        }
    }

    /// Ends the idle stretch in progress and keeps it when it is the longest so far.
    fn close(&mut self, run: &mut Run) {
        let ended = std::mem::take(run);
        let Some(cause) = ended.dominant() else {
            return;
        };
        let longer = |s: &Stretch| ended.to - ended.from > s.end_ms - s.start_ms;
        if self.longest.as_ref().is_none_or(longer) {
            self.longest = Some(Stretch {
                start_ms: ended.from,
                end_ms: ended.to,
                cause,
                label: cause.label(),
            });
        }
    }

    fn absorb(&mut self, other: Tally) {
        self.tracked_ms += other.tracked_ms;
        self.tracking_since = match (self.tracking_since, other.tracking_since) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.held_ms += other.held_ms;
        for (reason, ms) in other.held_by {
            *self.held_by.entry(reason).or_default() += ms;
        }
        self.offline_ms += other.offline_ms;
        self.used_ms += other.used_ms;
        self.queue_empty_ms += other.queue_empty_ms;
        self.work_slices.extend(other.work_slices);
        self.quota_slices.extend(other.quota_slices);
        let other_longer = match (&self.longest, &other.longest) {
            (Some(mine), Some(theirs)) => {
                theirs.end_ms - theirs.start_ms > mine.end_ms - mine.start_ms
            }
            (None, Some(_)) => true,
            _ => false,
        };
        if other_longer {
            self.longest = other.longest;
        }
    }

    fn finish(self) -> (GuardSummary, IdleSummary) {
        let mut held_by: Vec<HeldBy> = self
            .held_by
            .into_iter()
            .map(|(reason, ms)| HeldBy {
                reason,
                label: reason.label().to_string(),
                ms,
            })
            .collect();
        held_by.sort_by(|a, b| b.ms.cmp(&a.ms).then(a.reason.cmp(&b.reason)));
        let guard = GuardSummary {
            tracked_ms: self.tracked_ms,
            tracking_since_ms: self.tracking_since,
            held_ms: self.held_ms,
            held_by,
            offline_ms: self.offline_ms,
        };
        let idle = IdleSummary {
            used_ms: self.used_ms,
            queue_empty_ms: self.queue_empty_ms,
            work_waiting_ms: merge(self.work_slices).iter().map(|(a, b)| b - a).sum(),
            quota_waiting_ms: merge(self.quota_slices).iter().map(|(a, b)| b - a).sum(),
            stuck_jobs: Vec::new(),
            longest: self.longest,
        };
        (guard, idle)
    }
}
