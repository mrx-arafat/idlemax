//! Domain: when the dispatcher may launch one more worker (D9, D13): one launch per device at
//! a time with memory left to settle, inside the room, the account rule and the review cap,
//! which task may be launched, and what each account spent in its weekly window (D10 item 1,
//! A7). Pure.

use crate::dispatch::accounts::AccountUse;
use crate::dispatch::room::{LineState, RoomDecision};
use crate::types::{JobId, QuotaSnapshot, UnixMs};

use super::run::{Run, RunState};

const DAY_MS: UnixMs = 86_400_000;
/// The measuring week of an account lasts this long from its first Run (A7).
pub const MEASURING_WEEK_MS: UnixMs = 7 * DAY_MS;

/// Everything one launch decision reads, for one device.
pub struct LaunchGate<'a> {
    pub now_ms: UnixMs,
    /// Why the device takes nothing: offline, paused, kill switch (`DevicePlan.held`).
    pub held: Option<&'a str>,
    /// A paused scope holds the account.
    pub account_paused: bool,
    pub room: &'a RoomDecision,
    /// Every Run of the device.
    pub device_runs: &'a [Run],
    /// When the newest worker job of the device was created.
    pub last_launch_ms: Option<UnixMs>,
    pub settle_ms: UnixMs,
    /// The jobs the device reports running.
    pub reported: &'a [JobId],
    /// The CLI is listed in `workers.clis`.
    pub cli_allowed: bool,
    pub account: Option<&'a AccountUse>,
}

/// None when one more worker may launch now, else why not. `previous` is the job of the Run
/// being relaunched, which the device must no longer report.
pub fn may_launch(gate: &LaunchGate<'_>, previous: Option<JobId>) -> Option<String> {
    if let Some(why) = gate.held {
        return Some(format!("the device takes nothing: {why}"));
    }
    if gate.account_paused {
        return Some("the account is paused".into());
    }
    if gate.room.state != LineState::Run || gate.room.open_slots == 0 {
        let state = gate.room.state.as_str();
        return Some(format!(
            "no open worker slot ({state}: {})",
            gate.room.reason
        ));
    }
    if gate
        .device_runs
        .iter()
        .any(|r| r.state == RunState::Starting)
    {
        return Some("a worker on this device is still starting".into());
    }
    if gate
        .last_launch_ms
        .is_some_and(|t| gate.now_ms - t < gate.settle_ms)
    {
        return Some("memory is still settling after the last launch".into());
    }
    if previous.is_some_and(|job| gate.reported.contains(&job)) {
        return Some("the device still reports the previous worker".into());
    }
    if !gate.cli_allowed {
        return Some("its CLI is not in workers.clis".into());
    }
    match gate.account {
        Some(a) if a.usable => None,
        Some(a) => Some(format!("account {} is not usable: {}", a.account, a.why)),
        None => Some("the account is not configured".into()),
    }
}

/// Finished tasks of `project` waiting for the owner's review (`clean` and `needs_you`; a
/// `rejected` Run does not count, A24). The count Spec 4 shows.
pub fn waiting(runs: &[Run], project: &str) -> usize {
    runs.iter()
        .filter(|r| r.project == project)
        .filter(|r| matches!(r.state, RunState::Clean | RunState::NeedsYou))
        .count()
}

/// None when `project` has room under the cap for one more task: what waits for review
/// plus the work in flight, so work under way cannot push it past the cap (D9).
pub fn under_cap(runs: &[Run], project: &str, cap: u32) -> Option<String> {
    let in_flight = runs
        .iter()
        .filter(|r| r.project == project)
        .filter(|r| {
            matches!(
                r.state,
                RunState::Starting
                    | RunState::Running
                    | RunState::Checking
                    | RunState::Shed
                    | RunState::Stopped
            )
        })
        .count();
    let taken = waiting(runs, project) + in_flight;
    (taken >= cap as usize).then(|| {
        format!("{project} has {taken} tasks waiting or in flight, the cap is {cap}: review them with idlemax review")
    })
}

/// Whether a task may be launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOpen {
    Open,
    /// It has a Run in a state other than `discarded`, apart from one `rejected` Run, which
    /// leaves the task open for a second attempt (Spec 3 Task 10, D10.6).
    Busy,
    /// Its last two Runs ended `rejected` or `discarded`: never launched again.
    Closed,
}

pub fn task_open(runs: &[Run], task: &str) -> TaskOpen {
    let mut mine: Vec<&Run> = runs
        .iter()
        .filter(|r| r.task == task && !never_worked(r))
        .collect();
    mine.sort_by_key(|r| (r.start_ms, r.id));
    let failed = |r: &&Run| matches!(r.state, RunState::Rejected | RunState::Discarded);
    if mine.len() >= 2 && mine[mine.len() - 2..].iter().all(failed) {
        return TaskOpen::Closed;
    }
    let held: Vec<&&Run> = mine
        .iter()
        .filter(|r| r.state != RunState::Discarded)
        .collect();
    match held.as_slice() {
        [] => TaskOpen::Open,
        [only] if only.state == RunState::Rejected => TaskOpen::Open,
        _ => TaskOpen::Busy,
    }
}

/// A Run the owner discarded whose worker never reached `running` (and so has no verdict):
/// no try at its task and no spend of its account. One discarded mid-work still counts.
fn never_worked(r: &Run) -> bool {
    r.state == RunState::Discarded && r.running_ms.is_none() && r.verdict.is_none()
}

/// The weekly percent one account spent: its Runs in the window and the reviews it paid for,
/// merged into intervals that do not overlap, each adding the rise of the weekly reading
/// over it once, never below zero; the newest reading stands in for a Run still in flight.
/// `weekly` holds the account's weekly readings, oldest first.
pub fn week_spend(
    runs: &[Run],
    account: &str,
    weekly: &[QuotaSnapshot],
    window_start_ms: UnixMs,
    now_ms: UnixMs,
) -> f64 {
    let mut spans: Vec<(UnixMs, UnixMs)> = runs
        .iter()
        .filter(|r| r.account == account && r.launches > 0 && !never_worked(r))
        .map(|r| (r.start_ms, r.end_ms.unwrap_or(now_ms)))
        .collect();
    spans.extend(reviews_paid(runs, account));
    spans.retain(|(_, end)| *end >= window_start_ms);
    spans.sort_unstable();
    let mut merged: Vec<(UnixMs, UnixMs)> = Vec::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let at = |t: UnixMs| {
        let before = weekly.iter().rfind(|s| s.at_ms <= t);
        before.or(weekly.first()).map_or(0.0, |s| s.used_pct)
    };
    merged
        .iter()
        .map(|(start, end)| (at(*end) - at((*start).max(window_start_ms))).max(0.0))
        .sum()
}

/// The spans of the reviews `account` paid for, from the readings taken around each.
fn reviews_paid(runs: &[Run], account: &str) -> Vec<(UnixMs, UnixMs)> {
    let reviews = runs
        .iter()
        .filter_map(|r| r.facts.check.as_ref()?.review.as_ref())
        .filter(|rv| rv.account == account);
    reviews
        .filter_map(|rv| {
            let start = rv.quota_before.iter().map(|s| s.at_ms).min()?;
            let end = rv.quota_after.iter().map(|s| s.at_ms).max()?;
            Some((start, end))
        })
        .collect()
}

/// When the measuring week of an account ends (A7).
pub fn measuring_week_end(first_run_ms: UnixMs) -> UnixMs {
    first_run_ms + MEASURING_WEEK_MS
}

#[cfg(test)]
mod tests;
