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
use idlemax_core::worker::pace::{measuring_week_end, week_spend, MEASURING_WEEK_MS};
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
        measuring_left_pct: measuring_left(store, &a.name, now)?,
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

/// While the account's first Run is under seven days old: the 10 percent cap of A7 minus what
/// the Runs since then cost by Spec 2's `week_spend`; None after that, and for an account
/// with no Run yet.
fn measuring_left(store: &Store, account: &str, now: UnixMs) -> Result<Option<f64>, StoreError> {
    let runs = store.runs_of_account(account, 0)?;
    let Some(first) = runs.iter().map(|r| r.start_ms).min() else {
        return Ok(None);
    };
    if now >= measuring_week_end(first) {
        return Ok(None);
    }
    // The reading before the first Run is the base its rise is measured from.
    let weekly = store.weekly_readings(account, first - MEASURING_WEEK_MS)?;
    let spent = week_spend(&runs, account, &weekly, first, now);
    Ok(Some((MEASURING_WEEK_CAP_PCT - spent).max(0.0)))
}

/// The Budgets of one moment and the cost table behind them: what every worker start of a
/// launch step asks (Spec 3 Task 6), read once instead of once per candidate.
pub struct Gates {
    budgets: Vec<Budget>,
    table: CostTable,
    quota: QuotaConfig,
}

impl Gates {
    pub fn read(
        store: &Store,
        cfg: &Config,
        live: &Config,
        views: &[QuotaView],
        now: UnixMs,
    ) -> Result<Self, StoreError> {
        let (budgets, table) = budgets_with_table(store, cfg, live, views, now)?;
        Ok(Self {
            budgets,
            table,
            quota: live.quota.clone(),
        })
    }

    /// May one new Run of this account, model and task type start now.
    pub fn start(&self, account: &str, model: &str, task_type: &str) -> Gate {
        let found = self.budgets.iter().find(|b| b.account == account);
        gate(found, self.estimate(found, account, model, task_type))
    }

    /// May a Run back in the queue continue: its estimate already counts in the Budget's
    /// `committed_pct` (it has no end time), so it needs a usable Budget and no new room.
    pub fn relaunch(&self, account: &str) -> Gate {
        gate(self.budgets.iter().find(|b| b.account == account), 0.0)
    }

    /// 100 divided by the cap for an account on a daily cap, else `expected` (Task 3).
    fn estimate(&self, found: Option<&Budget>, account: &str, model: &str, task_type: &str) -> f64 {
        if let Some(cap) = found.and_then(|b| b.cap_runs) {
            return 100.0 / f64::from(cap.max(1));
        }
        let sample = RunSample {
            provider: found.map(|b| b.provider).unwrap_or_default(),
            account: account.to_string(),
            model: model.to_string(),
            task_type: task_type.to_string(),
            ..RunSample::default()
        };
        estimate(&self.table, &sample, &self.quota)
    }
}

/// May one worker Run of this account, model and task type start now.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
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
    Ok(Gates::read(store, cfg, live, views, now)?.start(account, model, task_type))
}
