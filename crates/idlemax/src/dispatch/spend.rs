//! Adapter: what each account spent in its current weekly window, from the stored Runs and
//! weekly readings through the pure `week_spend` (D10 item 1, A7). The plan and the launch
//! step read the same figure, so they never disagree about an account.

use idlemax_core::types::{QuotaWindowKind, UnixMs};
use idlemax_core::worker::pace::{week_spend, MEASURING_WEEK_MS};
use idlemax_core::Config;

use crate::coord::store::{Store, StoreError};

/// (account, weekly percent spent) for every configured account.
pub fn spent_week(
    store: &Store,
    cfg: &Config,
    now: UnixMs,
) -> Result<Vec<(String, f64)>, StoreError> {
    let latest = store.latest_quota()?;
    let runs = store.list_runs(None, None, 0)?;
    let mut spent = Vec::new();
    for account in &cfg.quota.accounts {
        let name = account.name.as_str();
        let reset = latest
            .iter()
            .find(|s| s.account == name && s.window == QuotaWindowKind::Weekly)
            .and_then(|s| s.resets_at_ms);
        let start = reset.map_or(now - MEASURING_WEEK_MS, |r| r - MEASURING_WEEK_MS);
        let readings = store.weekly_readings(name, start - MEASURING_WEEK_MS)?;
        spent.push((
            name.to_string(),
            week_spend(&runs, name, &readings, start, now),
        ));
    }
    Ok(spent)
}
