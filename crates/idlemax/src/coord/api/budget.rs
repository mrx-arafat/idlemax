//! Budget use cases on the coordinator (adapter): gathers the stored readings, Run samples
//! and account days and calls the pure `idlemax_core::quota` rules. Persists what they decide.

use idlemax_core::quota::owner_use::{account_day, closed_slots, AccountDay, DAY_MS};
use idlemax_core::quota::AccountConfig;
use idlemax_core::types::UnixMs;
use idlemax_core::Config;

use super::quota_views;
use crate::coord::store::{Store, StoreError};

pub(crate) mod gather;

pub use gather::budgets;

#[cfg(test)]
pub(crate) mod tests;

/// Snapshots older than a cycle plus a day cannot name a slot still to close.
const LOOKBACK_MS: i64 = 8 * DAY_MS;

/// Closes every ended account day that is not closed yet. A second call changes nothing.
pub fn close_quota_days(store: &mut Store, cfg: &Config, now: UnixMs) -> Result<(), StoreError> {
    for account in &cfg.quota.accounts {
        match account.daily_cap_runs {
            Some(cap) => close_cap_days(store, account, cap, now)?,
            None => close_weekly_days(store, cfg, account, now)?,
        }
    }
    Ok(())
}

fn close_weekly_days(
    store: &mut Store,
    cfg: &Config,
    account: &AccountConfig,
    now: UnixMs,
) -> Result<(), StoreError> {
    let since = now - LOOKBACK_MS;
    let weekly = store.weekly_snapshots(&account.name, since)?;
    let slots = closed_slots(&weekly, now);
    let Some(first) = slots.first() else {
        return Ok(());
    };
    let closed: Vec<UnixMs> = store
        .budget_days(&account.name, first.start_ms)?
        .into_iter()
        .filter(|d| d.closed)
        .map(|d| d.day_start_ms)
        .collect();
    let charges: Vec<(UnixMs, f64)> = gather::Samples::read(store, cfg, now)?
        .charges(&account.name, &cfg.quota)
        .into_iter()
        .filter_map(|(end, pct)| Some((end?, pct)))
        .collect();
    for slot in slots.iter().filter(|s| !closed.contains(&s.start_ms)) {
        store.close_day(&account_day(&account.name, slot, &weekly, &charges))?;
    }
    Ok(())
}

/// An account on a daily cap closes each open UTC day that ended: its Runs against the cap.
fn close_cap_days(
    store: &mut Store,
    account: &AccountConfig,
    cap: u32,
    now: UnixMs,
) -> Result<(), StoreError> {
    let open = store
        .budget_days(&account.name, now - LOOKBACK_MS)?
        .into_iter()
        .filter(|d| !d.closed && d.day_start_ms + DAY_MS <= now);
    for day in open.collect::<Vec<_>>() {
        let runs =
            store.runs_started_in(&account.name, day.day_start_ms, day.day_start_ms + DAY_MS)?;
        store.close_day(&AccountDay {
            idlemax_pct: Some(100.0 * f64::from(runs) / f64::from(cap.max(1))),
            runs: Some(runs),
            closed: true,
            total_pct: None,
            owner_pct: None,
            ..day
        })?;
    }
    Ok(())
}

/// Writes what each account's Budget says now into today's row, so a closed day keeps the
/// last budget recorded within its final hour.
pub fn record_open_days(
    store: &mut Store,
    cfg: &Config,
    live: &Config,
    now: UnixMs,
) -> Result<(), StoreError> {
    let views = quota_views(store, cfg, now)?;
    for b in budgets(store, cfg, live, &views, now)? {
        let spendable = Some(b.spendable_pct);
        store.record_open_day(
            &b.account,
            b.day_start_ms,
            spendable,
            b.basis.as_str(),
            &b.why,
        )?;
    }
    Ok(())
}
