use idlemax_core::protocol::Heartbeat;
use idlemax_core::quota::{AccountConfig, Provider};
use idlemax_core::types::{QuotaSnapshot, QuotaWindowKind};

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

mod gather_tests {
    use idlemax_core::quota::budget::BudgetBasis;
    use idlemax_core::quota::rolling_5h;
    use idlemax_core::worker::run::{Run, RunLaunch, RunState};

    use super::super::gather::{budgets, worker_gate};
    use super::super::record_open_days;
    use super::*;
    use crate::coord::api::quota_views;
    use crate::coord::store::UpdateWrites;

    const HOUR: i64 = 3_600_000;

    fn account(name: &str, provider: Provider, cap: Option<u32>) -> AccountConfig {
        AccountConfig {
            name: name.into(),
            provider,
            daily_cap_runs: cap,
            ..AccountConfig::default()
        }
    }

    /// Readings of `max` at `at`: the weekly window resets at `reset`.
    fn readings(store: &mut Store, at: UnixMs, weekly: f64, five: f64, reset: UnixMs) {
        let snap = |window, used, resets| QuotaSnapshot {
            account: "max".into(),
            window,
            used_pct: used,
            resets_at_ms: Some(resets),
            source: Default::default(),
            at_ms: at,
        };
        let mut hb = Heartbeat::default();
        hb.device.id = "mac".into();
        hb.quota = vec![
            snap(QuotaWindowKind::Weekly, weekly, reset),
            snap(rolling_5h(), five, at + 2 * HOUR),
        ];
        store.record_heartbeat(&hb, at).unwrap();
    }

    fn add_run(
        store: &mut Store,
        account: &str,
        start: UnixMs,
        end: Option<UnixMs>,
        session: bool,
    ) {
        let before = store.latest_quota().unwrap();
        let run = store
            .insert_run(
                &Run {
                    device: "mac".into(),
                    account: account.into(),
                    model: "sonnet".into(),
                    source: "roadmap".into(),
                    start_ms: start,
                    updated_ms: start,
                    ..Run::default()
                },
                &RunLaunch::default(),
            )
            .unwrap();
        let next = Run {
            state: RunState::Running,
            launches: 1,
            end_ms: end,
            session: session.then(|| "s".to_string()),
            quota_before: before
                .into_iter()
                .filter(|s| s.account == account)
                .collect(),
            ..run
        };
        assert!(store.update_run(&next, RunState::Starting).unwrap());
    }

    fn budgets_now(
        store: &Store,
        cfg: &Config,
        live: &Config,
        now: UnixMs,
    ) -> Vec<idlemax_core::quota::budget::Budget> {
        let views = quota_views(store, cfg, now).unwrap();
        budgets(store, cfg, live, &views, now).unwrap()
    }

    #[test]
    fn budgets_follow_the_config_order_and_the_live_settings() {
        let mut store = Store::open_in_memory().unwrap();
        readings(&mut store, T0, 40.0, 10.0, T0 + 72 * HOUR);
        let mut cfg = claude_cfg();
        cfg.quota.accounts.push(account("key", Provider::Api, None));
        let list = budgets_now(&store, &cfg, &cfg, T0);
        assert_eq!(list.len(), 2);
        assert_eq!(
            (list[0].account.as_str(), list[1].basis),
            ("max", BudgetBasis::NotForWorkers)
        );

        let mut live = cfg.clone();
        live.quota.safety_margin_pct = 20.0;
        live.quota
            .accounts
            .push(account("new", Provider::Claude, None));
        let changed = budgets_now(&store, &cfg, &live, T0);
        assert_eq!(changed.len(), 2);
        assert!(changed[0].spendable_pct < list[0].spendable_pct);
    }

    #[test]
    fn a_limit_on_a_used_up_week_rests_the_account_until_the_weekly_reset() {
        let mut store = Store::open_in_memory().unwrap();
        let reset = T0 + 30 * HOUR;
        readings(&mut store, T0, 97.0, 40.0, reset);
        let writes = UpdateWrites {
            limit_hit: Some("max"),
            job: None,
            run: None,
            events: &[],
        };
        store.commit_update("mac", 1, &writes, T0).unwrap();
        let b = &budgets_now(&store, &claude_cfg(), &claude_cfg(), T0)[0];
        assert_eq!(
            (b.basis, b.rests_until_ms),
            (BudgetBasis::Resting, Some(reset))
        );
    }

    #[test]
    fn the_measuring_week_caps_the_first_seven_days() {
        let mut store = Store::open_in_memory().unwrap();
        let reset = T0 + 2 * DAY_MS;
        let start = T0 - 3 * DAY_MS;
        readings(&mut store, start, 40.0, 5.0, reset);
        add_run(&mut store, "max", start, Some(start + HOUR), true);
        // At the Run's end, so Spec 2's `week_spend` reads the rise of 6.
        readings(&mut store, start + HOUR, 46.0, 11.0, reset);
        readings(&mut store, T0, 50.0, 10.0, reset);
        let b = &budgets_now(&store, &claude_cfg(), &claude_cfg(), T0)[0];
        assert_eq!(b.basis, BudgetBasis::Measuring, "{b:?}");
        assert!(b.spendable_pct <= b.spent_pct + 4.0 + 1e-9, "{b:?}");

        let mut late = Store::open_in_memory().unwrap();
        readings(&mut late, T0, 50.0, 10.0, reset);
        add_run(
            &mut late,
            "max",
            T0 - 8 * DAY_MS,
            Some(T0 - 8 * DAY_MS + HOUR),
            true,
        );
        let b = &budgets_now(&late, &claude_cfg(), &claude_cfg(), T0)[0];
        assert_ne!(b.basis, BudgetBasis::Measuring, "{b:?}");
    }

    #[test]
    fn a_daily_cap_counts_todays_runs_with_a_session() {
        let mut store = Store::open_in_memory().unwrap();
        let day_start = T0 - T0.rem_euclid(DAY_MS);
        add_run(&mut store, "oc", day_start + HOUR, None, true);
        add_run(&mut store, "oc", day_start - HOUR, None, true);
        add_run(&mut store, "oc", day_start + 2 * HOUR, None, false);
        let mut cfg = Config::default();
        cfg.quota.accounts = vec![account("oc", Provider::Opencode, Some(5))];
        let b = &budgets_now(&store, &cfg, &cfg, T0)[0];
        assert_eq!(b.runs_today, 1);
    }

    #[test]
    fn the_worker_gate_and_todays_recorded_budget() {
        let mut store = Store::open_in_memory().unwrap();
        readings(&mut store, T0, 40.0, 10.0, T0 + 72 * HOUR);
        let cfg = claude_cfg();
        let views = quota_views(&store, &cfg, T0).unwrap();
        let gate = |live: &Config| {
            worker_gate(&store, &cfg, live, &views, "max", "sonnet", "roadmap", T0).unwrap()
        };
        assert!(gate(&cfg).allowed);
        let mut dear = cfg.clone();
        dear.quota.seed_standard_pct = 20.0;
        let refused = gate(&dear);
        assert!(
            !refused.allowed && refused.reason.starts_with("needs 20.0%"),
            "{refused:?}"
        );

        record_open_days(&mut store, &cfg, &cfg, T0).unwrap();
        let today = &store.budget_days("max", 0).unwrap()[0];
        assert!(today.spendable_pct.is_some() && today.basis.is_some() && today.why.is_some());
    }
}

#[tokio::test]
async fn the_quota_route_carries_each_accounts_budget_on_its_rows() {
    use crate::coord::api::tests::{mac_heartbeat, Api};
    use crate::state::now_ms;
    use idlemax_core::quota::rolling_5h;
    let api = Api::start_with(claude_cfg()).await;
    let now = now_ms();
    let snap = |window, used| QuotaSnapshot {
        account: "max".into(),
        window,
        used_pct: used,
        resets_at_ms: Some(now + 2 * DAY_MS),
        source: Default::default(),
        at_ms: now,
    };
    let mut hb = mac_heartbeat(Vec::new());
    hb.quota = vec![
        snap(rolling_5h(), 10.0),
        snap(QuotaWindowKind::Weekly, 30.0),
    ];
    api.post("/v1/agent/heartbeat", &hb).await;
    let rows: Vec<serde_json::Value> = api
        .get_as("/v1/quota", "localhost")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["budget"], rows[1]["budget"]);
    let b = &rows[0]["budget"];
    assert!(b["spendable_pct"].is_number() && b["spent_pct"].is_number() && b["why"].is_string());
    let costs = b["costs"].as_array().unwrap();
    assert_eq!(costs.len(), 2);
    for c in costs {
        assert!(
            c["est_pct"].is_number() && c["basis"].is_string() && c["samples"].is_number(),
            "{c}"
        );
    }
}
