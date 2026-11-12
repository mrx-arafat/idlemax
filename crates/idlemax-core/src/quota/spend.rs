//! Domain: the rules that decide when a worker may spend an account, in one place. Pure.
//!
//! | Rule | Old AI job (`cost = ai`, no `worker`) | Worker Run |
//! |---|---|---|
//! | May it start | `quota::rules`: spendable, final third or last day, interactive hold | `gate`: the day's Budget has room for the Run's estimate |
//! | Cost of one run | fixed `token_class_pct` (3, 8, 20) | `quota::cost`: measured, else the tier's seed |
//! | One at a time per account | yes (one AI lease, one start per 5 hours) | no (A8) |
//! | 5-hour line | the window's reserve | `five_hour_stop_pct`: no start, and running workers stop (A18) |
//! | After a usage limit | `limit_block`: blocked until the window that saw it resets | `limit_rest_until`: rest until the used-up window resets, workers stop (D10.7) |
//! | agy, OpenCode | fixed windows | a daily cap of Runs (D10.8) |

use super::budget::{Budget, BudgetBasis};
use super::rules::duration;
use super::{window_minutes, AccountConfig, QuotaView, WindowState};
use crate::quota::owner_use::utc_day_slot;
use crate::types::UnixMs;

#[cfg(test)]
mod tests;

/// A window at or above this percent counts as the one the limit used up.
pub const LIMIT_USED_UP_PCT: f64 = 90.0;
const NO_WINDOW_REST_MINUTES: i64 = 300;
const MINUTE_MS: i64 = 60_000;

/// Whether one worker Run may start, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    pub allowed: bool,
    pub reason: String,
}

/// May one worker Run with this estimate start on the account now.
pub fn gate(budget: Option<&Budget>, est_pct: f64) -> Gate {
    let refuse = |reason: String| Gate {
        allowed: false,
        reason,
    };
    let Some(b) = budget else {
        return refuse("no budget for this account".into());
    };
    if !b.usable {
        return refuse(b.why.clone());
    }
    if est_pct > b.left_pct {
        return refuse(format!(
            "needs {est_pct:.1}%, {:.1}% left today",
            b.left_pct
        ));
    }
    Gate {
        allowed: true,
        reason: format!("{:.1}% left today, the run needs {est_pct:.1}%", b.left_pct),
    }
}

/// The bases at which the account's running workers stop with their state saved.
pub fn stops_workers(basis: BudgetBasis) -> bool {
    matches!(basis, BudgetBasis::Resting | BudgetBasis::FiveHourStop)
}

/// The usable Budgets in the order given, the ones the owner is not using first.
pub fn offers(budgets: &[Budget]) -> Vec<&Budget> {
    let usable = budgets.iter().filter(|b| b.usable);
    let (idle, busy): (Vec<&Budget>, Vec<&Budget>) = usable.partition(|b| !b.owner_active);
    idle.into_iter().chain(busy).collect()
}

/// Until when a limit hit at `hit_ms` rests the account; None when it rests no longer.
pub fn limit_rest_until(
    account: &AccountConfig,
    windows: &[QuotaView],
    hit_ms: UnixMs,
    now_ms: UnixMs,
) -> Option<UnixMs> {
    if account.daily_cap_runs.is_some() {
        return Some(utc_day_slot(hit_ms).end_ms).filter(|u| *u > now_ms);
    }
    let len = |v: &QuotaView| {
        v.window
            .map_or(0, |w| i64::from(window_minutes(w)) * MINUTE_MS)
    };
    let known: Vec<&QuotaView> = windows
        .iter()
        .filter(|v| v.resets_at_ms.is_some())
        .collect();
    let until = if known.is_empty() {
        let shortest = windows.iter().map(len).filter(|l| *l > 0).min();
        hit_ms + shortest.unwrap_or(NO_WINDOW_REST_MINUTES * MINUTE_MS)
    } else {
        let running: Vec<&QuotaView> = known
            .into_iter()
            .filter(|v| {
                v.resets_at_ms
                    .is_some_and(|r| hit_ms > r - len(v) && hit_ms <= r)
            })
            .collect();
        let used_up: Vec<&QuotaView> = running
            .iter()
            .copied()
            .filter(|v| v.state == WindowState::Active)
            .filter(|v| v.used_pct.is_some_and(|u| u >= LIMIT_USED_UP_PCT))
            .collect();
        let chosen = if used_up.is_empty() {
            running
                .iter()
                .copied()
                .min_by_key(|v| len(v))
                .into_iter()
                .collect()
        } else {
            used_up
        };
        chosen.iter().filter_map(|v| v.resets_at_ms).max()?
    };
    (until > now_ms).then_some(until)
}

/// `claude-max: usage limit hit, workers rest for 3h 10m`.
pub fn rest_event(account: &str, until_ms: UnixMs, now_ms: UnixMs) -> String {
    format!(
        "{account}: usage limit hit, workers rest for {}",
        duration(until_ms - now_ms)
    )
}
