//! Domain: the rules that decide when a worker may spend an account, in one place. Pure.
//! The same ten rows stand in the README's "AI quota maximizer" section.
//!
//! | Rule (where it lives) | Old AI job | Worker |
//! |---|---|---|
//! | The device lists the account (`scheduler/blockers.rs`) | applies | applies: the plan picks only an account the device lists |
//! | `idlemax pause --account` (`scheduler/blockers.rs`) | applies | applies, basis `paused`; a running worker is frozen in place |
//! | Usage data is trusted and under 6 hours old (`quota/rules.rs`) | applies | applies to a start, basis `no_data` |
//! | A usage limit or HTTP 429 was seen (`limit_block`) | applies, unchanged | `limit_rest_until`, basis `resting`; running workers stop |
//! | Reserve of 40% per 5-hour window, 30% weekly, 15% in the final day | applies | replaced by the surplus, the 5-hour stop (running workers stop too) and the last-day floor |
//! | Only in the final third of the shortest window or within 24 h of the weekly reset | applies | does not apply: the Budget paces by account day |
//! | Hold for 30 minutes after interactive use | applies | does not apply; it becomes the preference of `offers` |
//! | One AI lease per account | applies among old AI jobs | does not apply, in both directions (A8) |
//! | One AI job per account per 5 hours | applies among old AI jobs | does not apply, and a worker start does not begin the span |
//! | Headroom for the job's token class (`token_class_pct`) | applies, with the fixed figures | replaced by `gate`: the Run's estimate fits in `left_pct` |

use super::budget::{Budget, BudgetBasis};
use super::rules::duration;
use super::{window_minutes, AccountConfig, QuotaView, WindowState};
use crate::quota::owner_use::utc_day_slot;
use crate::types::{QuotaWindowKind, UnixMs};

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

/// Until when a limit hit at `hit_ms` rests the account; None when it rests no longer. The
/// limit is in the windows a reading shows used up, and they rest it until their reset;
/// with none used up it is the shortest window the account has (the 5-hour one for Claude,
/// even before it has a reading), never the weekly one by default.
pub fn limit_rest_until(
    account: &AccountConfig,
    windows: &[QuotaView],
    hit_ms: UnixMs,
    now_ms: UnixMs,
) -> Option<UnixMs> {
    if account.daily_cap_runs.is_some() {
        return Some(utc_day_slot(hit_ms).end_ms).filter(|u| *u > now_ms);
    }
    let len = |k: QuotaWindowKind| i64::from(window_minutes(k)) * MINUTE_MS;
    // The window's reset when the hit falls inside its current cycle.
    let reset_of = |v: &QuotaView| {
        let r = v.resets_at_ms?;
        let k = v.window?;
        (hit_ms > r - len(k) && hit_ms <= r).then_some(r)
    };
    let used_up = windows
        .iter()
        .filter(|v| v.state == WindowState::Active)
        .filter(|v| v.used_pct.is_some_and(|u| u >= LIMIT_USED_UP_PCT))
        .filter_map(reset_of)
        .max();
    let until = match used_up {
        Some(r) => r,
        None => {
            let kinds = account.effective_windows().into_iter().map(|c| c.kind);
            let shortest = kinds
                .chain(windows.iter().filter_map(|v| v.window))
                .min_by_key(|k| window_minutes(*k));
            match shortest {
                Some(k) => windows
                    .iter()
                    .filter(|v| v.window == Some(k))
                    .find_map(reset_of)
                    .unwrap_or(hit_ms + len(k)),
                None => hit_ms + NO_WINDOW_REST_MINUTES * MINUTE_MS,
            }
        }
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
