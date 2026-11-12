//! Gathers every account's Budget on the coordinator (adapter): the readings, the account
//! days, the Run samples, the pauses, the limit hits and the owner's CLI use, handed to the
//! pure `quota::budget`. `cfg` gives the accounts, `live` every other `[quota]` key (A2).

use idlemax_core::control::paused_accounts;
use idlemax_core::dispatch::accounts::MEASURING_WEEK_CAP_PCT;
use idlemax_core::quota::budget::{budget, Budget, BudgetInput};
use idlemax_core::quota::cost::{
    cost_table, expected, run_costs, tier_costs, CostKey, CostTable, RunCost, RunSample,
};
use idlemax_core::quota::owner_use::{day_slot, owner_use, utc_day_slot, DaySlot, DAY_MS};
use idlemax_core::quota::spend::{gate, Gate};
use idlemax_core::quota::{tier_of, AccountConfig, QuotaConfig, QuotaView, Tier};
use idlemax_core::types::{QuotaWindowKind, UnixMs};
use idlemax_core::Config;

use crate::coord::store::{Store, StoreError};

const WEEK_MS: i64 = 7 * DAY_MS;
/// Closed days `owner_use` may read.
const DAYS_KEPT_MS: i64 = 30 * DAY_MS;

/// The Run samples of the snapshot retention and their costs, read once per call.
pub(super) struct Samples {
    samples: Vec<RunSample>,
    costs: Vec<RunCost>,
    pub(super) table: CostTable,
}

impl Samples {
    pub(super) fn read(store: &Store, cfg: &Config, now: UnixMs) -> Result<Self, StoreError> {
        let retention = i64::from(cfg.coordinator.metrics_retention_days).saturating_mul(DAY_MS);
        let samples = store.run_samples_since(now.saturating_sub(retention))?;
        let costs = run_costs(&samples);
        let table = cost_table(&samples, &costs);
        Ok(Self {
            samples,
            costs,
            table,
        })
    }

    /// (end time, percent of the weekly window) of each Run of the account: measured where
    /// the table measured it, at its estimate otherwise, never at zero. None while it runs.
    pub(super) fn charges(&self, account: &str, q: &QuotaConfig) -> Vec<(Option<UnixMs>, f64)> {
        self.samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.account == account)
            .map(|(i, s)| {
                let measured = self
                    .costs
                    .iter()
                    .find(|c| c.sample == i && c.window == QuotaWindowKind::Weekly)
                    .and_then(|c| c.pct);
                (
                    s.ended_ms,
                    measured.unwrap_or_else(|| estimate(&self.table, s, q)),
                )
            })
            .collect()
    }
}

fn estimate(table: &CostTable, s: &RunSample, q: &QuotaConfig) -> f64 {
    let key = CostKey {
        provider: s.provider,
        account: s.account.clone(),
        model: s.model.clone(),
        task_type: s.task_type.clone(),
    };
    let tier = tier_of(s.provider, &s.model, q).unwrap_or(Tier::Standard);
    expected(table, &key, tier, q).pct
}

/// What every Budget of one call reads.
struct Shared {
    samples: Samples,
    paused: Vec<String>,
    hits: Vec<(String, UnixMs)>,
    interactive: Vec<(String, UnixMs)>,
}

/// One Budget per entry of `[[quota.accounts]]`, in that order, and the cost table.
pub fn budgets_with_table(
    store: &Store,
    cfg: &Config,
    live: &Config,
    views: &[QuotaView],
    now: UnixMs,
) -> Result<(Vec<Budget>, CostTable), StoreError> {
    let shared = Shared {
        samples: Samples::read(store, cfg, now)?,
        paused: paused_accounts(&store.paused_scopes()?),
        hits: store.limit_hits(now.saturating_sub(WEEK_MS))?,
        interactive: store.interactive_use()?,
    };
    let list = cfg
        .quota
        .accounts
        .iter()
        .map(|a| account_budget(store, &live.quota, a, views, &shared, now))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((list, shared.samples.table))
}

pub fn budgets(
    store: &Store,
    cfg: &Config,
    live: &Config,
    views: &[QuotaView],
    now: UnixMs,
) -> Result<Vec<Budget>, StoreError> {
    Ok(budgets_with_table(store, cfg, live, views, now)?.0)
}

/// The account day a Budget is for.
fn slot_of(a: &AccountConfig, windows: &[QuotaView], now: UnixMs) -> DaySlot {
    let reset = windows
        .iter()
        .find(|v| v.window == Some(QuotaWindowKind::Weekly))
        .and_then(|v| v.resets_at_ms);
    match (a.daily_cap_runs, reset) {
        (None, Some(r)) => day_slot(r, now),
        _ => utc_day_slot(now),
    }
}

fn account_budget(
    store: &Store,
    q: &QuotaConfig,
    a: &AccountConfig,
    views: &[QuotaView],
    shared: &Shared,
    now: UnixMs,
) -> Result<Budget, StoreError> {
    let windows: Vec<QuotaView> = views
        .iter()
        .filter(|v| v.account == a.name)
        .cloned()
        .collect();
    let slot = slot_of(a, &windows, now);
    let charges = shared.samples.charges(&a.name, q);
    let ended_in = |from, to| ended_between(&charges, from, to);
    let committed: f64 = charges
        .iter()
        .filter(|(end, _)| end.is_none())
        .map(|(_, p)| p)
        .sum();
    let cycle_start = slot.end_ms + i64::from(slot.days_left.saturating_sub(1)) * DAY_MS - WEEK_MS;
    let costs = tier_costs(a.provider, &a.name, &shared.samples.table, q);
    Ok(budget(&BudgetInput {
        now_ms: now,
        account: a,
        cfg: q,
        windows: &windows,
        owner: owner_use(&store.budget_days(&a.name, now - DAYS_KEPT_MS)?),
        idlemax_week_pct: ended_in(cycle_start, now),
        spent_today_pct: ended_in(slot.start_ms, slot.end_ms),
        committed_pct: committed,
        runs_today: store.runs_started_in(&a.name, slot.start_ms, slot.end_ms)?,
        measuring_left_pct: measuring_left(store, &a.name, &charges, now)?,
        owner_reserve_override_pct: None,
        limit_hit_ms: shared
            .hits
            .iter()
            .find(|(n, _)| *n == a.name)
            .map(|(_, t)| *t),
        paused: shared.paused.contains(&a.name),
        last_owner_use_ms: last_use(&shared.interactive, a),
        costs: &costs,
    }))
}

/// What the Runs that ended in (from, to] cost.
fn ended_between(charges: &[(Option<UnixMs>, f64)], from: UnixMs, to: UnixMs) -> f64 {
    charges
        .iter()
        .filter(|(end, _)| end.is_some_and(|e| e > from && e <= to))
        .map(|(_, pct)| pct)
        .sum()
}

/// The owner's newest use of the account's provider CLI.
fn last_use(interactive: &[(String, UnixMs)], a: &AccountConfig) -> Option<UnixMs> {
    let cli = a.provider.cli()?;
    interactive
        .iter()
        .filter(|(c, _)| c == cli)
        .map(|(_, t)| *t)
        .max()
}

// SHORTCUT: the measuring week is read from the Run records here, because Spec 2's
// `pace::week_spend` and `pace::measuring_week_end` are not on this branch yet; call them
// instead once Spec 2 Task 4 lands.
/// While the account's first Run is under seven days old: the 10 percent cap of A7 minus what
/// the Runs since then cost; None after that, and for an account with no Run yet.
fn measuring_left(
    store: &Store,
    account: &str,
    charges: &[(Option<UnixMs>, f64)],
    now: UnixMs,
) -> Result<Option<f64>, StoreError> {
    let Some(first) = store
        .runs_of_account(account, 0)?
        .first()
        .map(|r| r.start_ms)
    else {
        return Ok(None);
    };
    if now >= first + WEEK_MS {
        return Ok(None);
    }
    let spent: f64 = charges
        .iter()
        .filter(|(end, _)| end.is_some_and(|e| e >= first))
        .map(|(_, pct)| pct)
        .sum();
    Ok(Some((MEASURING_WEEK_CAP_PCT - spent).max(0.0)))
}

/// May one worker Run of this account, model and task type start now.
#[allow(clippy::too_many_arguments)]
// Spec 2's `launch_due` asks it before every worker start (Spec 3 Task 6).
#[cfg_attr(not(test), allow(dead_code))]
pub fn worker_gate(
    store: &Store,
    cfg: &Config,
    live: &Config,
    views: &[QuotaView],
    account: &str,
    model: &str,
    task_type: &str,
    now: UnixMs,
) -> Result<Gate, StoreError> {
    let (list, table) = budgets_with_table(store, cfg, live, views, now)?;
    let found = list.iter().find(|b| b.account == account);
    let est = match found.and_then(|b| b.cap_runs) {
        Some(cap) => 100.0 / f64::from(cap.max(1)),
        None => {
            let provider = found.map(|b| b.provider).unwrap_or_default();
            let sample = RunSample {
                provider,
                account: account.to_string(),
                model: model.to_string(),
                task_type: task_type.to_string(),
                ..RunSample::default()
            };
            estimate(&table, &sample, &live.quota)
        }
    };
    Ok(gate(found, est))
}
