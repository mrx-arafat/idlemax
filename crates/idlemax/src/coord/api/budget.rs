//! Budget use cases on the coordinator (adapter): gathers the stored readings, Run samples
//! and account days and calls the pure `idlemax_core::quota` rules. Persists what they decide.

use idlemax_core::quota::cost::{cost_table, expected, run_costs, CostKey};
use idlemax_core::quota::owner_use::{account_day, closed_slots, AccountDay, DAY_MS};
use idlemax_core::quota::{tier_of, AccountConfig, QuotaConfig, Tier};
use idlemax_core::types::{QuotaWindowKind, UnixMs};
use idlemax_core::Config;

use crate::coord::store::{Store, StoreError};

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
    let charges = charges(store, cfg, &account.name, now)?;
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

/// (end time, percent of the weekly window) of the account's ended Runs: measured where the
/// cost table measured them, at their estimate otherwise, never at zero.
pub(super) fn charges(
    store: &Store,
    cfg: &Config,
    account: &str,
    now: UnixMs,
) -> Result<Vec<(UnixMs, f64)>, StoreError> {
    let retention = i64::from(cfg.coordinator.metrics_retention_days).saturating_mul(DAY_MS);
    let samples = store.run_samples_since(now.saturating_sub(retention))?;
    let costs = run_costs(&samples);
    let table = cost_table(&samples, &costs);
    let q: &QuotaConfig = &cfg.quota;
    Ok(samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.account == account)
        .filter_map(|(i, s)| {
            let end = s.ended_ms?;
            let measured = costs
                .iter()
                .find(|c| c.sample == i && c.window == QuotaWindowKind::Weekly)
                .and_then(|c| c.pct);
            let pct = measured.unwrap_or_else(|| {
                let key = CostKey {
                    provider: s.provider,
                    account: s.account.clone(),
                    model: s.model.clone(),
                    task_type: s.task_type.clone(),
                };
                let tier = tier_of(s.provider, &s.model, q).unwrap_or(Tier::Standard);
                expected(&table, &key, tier, q).pct
            });
            Some((end, pct))
        })
        .collect())
}
