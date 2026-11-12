use idlemax_core::protocol::Heartbeat;
use idlemax_core::quota::{AccountConfig, Provider};
use idlemax_core::types::QuotaSnapshot;

use super::*;

pub(crate) const T0: UnixMs = 1_800_000_000_000;

/// A coordinator config with one Claude account.
pub(crate) fn claude_cfg() -> Config {
    let mut cfg = Config::default();
    cfg.quota.accounts = vec![AccountConfig {
        name: "max".into(),
        provider: Provider::Claude,
        ..AccountConfig::default()
    }];
    cfg
}

/// Weekly readings every 12 hours of the last three days of a cycle that resets in a day.
pub(crate) fn record_week(store: &mut Store) {
    let reset = T0 + DAY_MS;
    let mut hb = Heartbeat::default();
    hb.device.id = "mac".into();
    for i in 0..6 {
        let at = T0 - 3 * DAY_MS + i * DAY_MS / 2;
        hb.quota = vec![QuotaSnapshot {
            account: "max".into(),
            window: QuotaWindowKind::Weekly,
            used_pct: 10.0 + i as f64 * 3.0,
            resets_at_ms: Some(reset),
            source: Default::default(),
            at_ms: at,
        }];
        store.record_heartbeat(&hb, at).unwrap();
    }
}

#[test]
fn ended_account_days_close_once_and_old_ones_are_pruned() {
    let mut store = Store::open_in_memory().unwrap();
    let cfg = claude_cfg();
    record_week(&mut store);
    close_quota_days(&mut store, &cfg, T0).unwrap();
    let first = store.budget_days("max", 0).unwrap();
    close_quota_days(&mut store, &cfg, T0).unwrap();
    let second = store.budget_days("max", 0).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 6, "{first:?}");
    assert!(first.iter().all(|d| d.closed));
    assert!(first.iter().any(|d| d.owner_pct == Some(6.0)), "{first:?}");

    store
        .record_open_day("max", T0 - 40 * DAY_MS, Some(5.0), "learned", "old")
        .unwrap();
    store.prune(T0, 7 * DAY_MS).unwrap();
    assert!(store
        .budget_days("max", 0)
        .unwrap()
        .iter()
        .all(|d| d.day_start_ms > T0 - 30 * DAY_MS));
}
