//! Domain: account days and the owner's own use (D10.2). An account day is a 24-hour slot
//! counted back from the weekly reset (A25), so it needs no time zone; the owner's use of a
//! day is what the weekly reading rose minus what idlemax's Runs took. Pure.

use serde::{Deserialize, Serialize};

use super::SAME_RESET_MS;
use crate::types::{QuotaSnapshot, UnixMs};

#[cfg(test)]
mod tests;

pub const DAY_MS: i64 = 86_400_000;
/// Closed days `owner_use` averages over.
const KNOWN_DAYS: usize = 7;
const WEEK_DAYS: i64 = 7;

/// One account day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaySlot {
    pub start_ms: UnixMs,
    pub end_ms: UnixMs,
    /// Account days until the reset, this one included.
    pub days_left: u32,
}

/// days_left = ceil((resets_at_ms - now_ms) / DAY_MS), at least 1; the slot ends
/// (days_left - 1) days before the reset.
pub fn day_slot(resets_at_ms: UnixMs, now_ms: UnixMs) -> DaySlot {
    let ahead = (resets_at_ms - now_ms).max(1);
    let days_left = (ahead + DAY_MS - 1) / DAY_MS;
    let end_ms = resets_at_ms - (days_left - 1) * DAY_MS;
    DaySlot {
        start_ms: end_ms - DAY_MS,
        end_ms,
        days_left: u32::try_from(days_left.max(1)).unwrap_or(u32::MAX),
    }
}

/// The UTC day of `now_ms`, for an account with no reset time; days_left is 1.
pub fn utc_day_slot(now_ms: UnixMs) -> DaySlot {
    let start_ms = now_ms - now_ms.rem_euclid(DAY_MS);
    DaySlot {
        start_ms,
        end_ms: start_ms + DAY_MS,
        days_left: 1,
    }
}

/// Every day slot of the cycles the snapshots name that has ended by `now_ms`, oldest first.
pub fn closed_slots(weekly: &[QuotaSnapshot], now_ms: UnixMs) -> Vec<DaySlot> {
    let mut resets: Vec<UnixMs> = Vec::new();
    for r in weekly.iter().filter_map(|s| s.resets_at_ms) {
        if !resets.iter().any(|x| (x - r).abs() <= SAME_RESET_MS) {
            resets.push(r);
        }
    }
    let mut slots: Vec<DaySlot> = resets
        .into_iter()
        .flat_map(|reset| {
            (0..WEEK_DAYS).map(move |k| DaySlot {
                start_ms: reset - (k + 1) * DAY_MS,
                end_ms: reset - k * DAY_MS,
                days_left: u32::try_from(k + 1).unwrap_or(1),
            })
        })
        .filter(|s| s.end_ms <= now_ms)
        .collect();
    slots.sort_by_key(|s| s.start_ms);
    slots
}

/// One row of `quota_days`. The three budget fields are what the day's Budget last said
/// (Task 6 writes them); `basis` is the wire text of the Budget's basis.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountDay {
    pub account: String,
    pub day_start_ms: UnixMs,
    pub spendable_pct: Option<f64>,
    pub basis: Option<String>,
    pub why: Option<String>,
    pub total_pct: Option<f64>,
    pub idlemax_pct: Option<f64>,
    pub owner_pct: Option<f64>,
    pub runs: Option<u32>,
    pub closed: bool,
}

/// A closed day: the weekly rise across the slot, what idlemax's Runs that ended in it took
/// (`charges` of (end time, percent)), and the rest as the owner's.
pub fn account_day(
    account: &str,
    slot: &DaySlot,
    weekly: &[QuotaSnapshot],
    charges: &[(UnixMs, f64)],
) -> AccountDay {
    let inside = |at: UnixMs| at > slot.start_ms && at <= slot.end_ms;
    let end = weekly
        .iter()
        .filter(|s| inside(s.at_ms))
        .max_by_key(|s| s.at_ms);
    let total_pct = end.map(|end| {
        let same_cycle = |s: &&QuotaSnapshot| match (s.resets_at_ms, end.resets_at_ms) {
            (Some(a), Some(b)) => (a - b).abs() <= SAME_RESET_MS,
            _ => false,
        };
        let start = weekly
            .iter()
            .filter(|s| s.at_ms <= slot.start_ms)
            .filter(same_cycle)
            .max_by_key(|s| s.at_ms)
            .map_or(0.0, |s| s.used_pct);
        (end.used_pct - start).max(0.0)
    });
    let mine: Vec<f64> = charges
        .iter()
        .filter(|(at, _)| inside(*at))
        .map(|(_, pct)| *pct)
        .collect();
    let idlemax_pct: f64 = mine.iter().sum();
    AccountDay {
        account: account.to_string(),
        day_start_ms: slot.start_ms,
        total_pct,
        idlemax_pct: Some(idlemax_pct),
        owner_pct: total_pct.map(|t| (t - idlemax_pct).max(0.0)),
        runs: Some(u32::try_from(mine.len()).unwrap_or(u32::MAX)),
        closed: true,
        ..AccountDay::default()
    }
}

/// The owner's usual use per account day.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OwnerUse {
    pub days_known: u32,
    pub usual_daily_pct: Option<f64>,
}

/// The mean `owner_pct` of the newest 7 closed days that have one.
pub fn owner_use(days: &[AccountDay]) -> OwnerUse {
    let mut known: Vec<&AccountDay> = days
        .iter()
        .filter(|d| d.closed && d.owner_pct.is_some())
        .collect();
    known.sort_by_key(|d| std::cmp::Reverse(d.day_start_ms));
    known.truncate(KNOWN_DAYS);
    let values: Vec<f64> = known.iter().filter_map(|d| d.owner_pct).collect();
    OwnerUse {
        days_known: u32::try_from(values.len()).unwrap_or(u32::MAX),
        usual_daily_pct: (!values.is_empty())
            .then(|| values.iter().sum::<f64>() / values.len() as f64),
    }
}
