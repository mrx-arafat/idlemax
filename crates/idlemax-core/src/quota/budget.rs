//! Domain: the Budget, the quota idlemax may spend on one account today and why that number
//! (D10.2 to D10.5, D10.8, A7, A18). Pure; time is `now_ms`. Every percent is percent of the
//! weekly window, except for an account on a daily cap, where it is percent of the day's cap.

use serde::{Deserialize, Serialize};

use super::cost::TierCost;
use super::owner_use::{day_slot, utc_day_slot, DaySlot, OwnerUse};
use super::rules::{duration, INTERACTIVE_HOLD_MS};
use super::spend::limit_rest_until;
use super::surplus::{hold, surplus};
use super::{rolling_5h, AccountConfig, Provider, QuotaConfig, QuotaView, WindowState};
use crate::types::{QuotaWindowKind, UnixMs};

#[cfg(test)]
pub(crate) mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetUnit {
    WeeklyPct,
    DailyCapRuns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetBasis {
    // usable
    Learned,
    Reserve,
    LastDay,
    Measuring,
    DailyCap,
    // closed
    Paused,
    Resting,
    NoData,
    NoCap,
    NotForWorkers,
    FiveHourStop,
    Spent,
}

impl BudgetBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetBasis::Learned => "learned",
            BudgetBasis::Reserve => "reserve",
            BudgetBasis::LastDay => "last_day",
            BudgetBasis::Measuring => "measuring",
            BudgetBasis::DailyCap => "daily_cap",
            BudgetBasis::Paused => "paused",
            BudgetBasis::Resting => "resting",
            BudgetBasis::NoData => "no_data",
            BudgetBasis::NoCap => "no_cap",
            BudgetBasis::NotForWorkers => "not_for_workers",
            BudgetBasis::FiveHourStop => "five_hour_stop",
            BudgetBasis::Spent => "spent",
        }
    }
}

/// The quota idlemax may spend on one account today.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub account: String,
    pub provider: Provider,
    pub unit: BudgetUnit,
    pub day_start_ms: UnixMs,
    pub day_end_ms: UnixMs,
    pub spendable_pct: f64,
    pub spent_pct: f64,
    pub left_pct: f64,
    pub usable: bool,
    pub rests_until_ms: Option<UnixMs>,
    pub owner_active: bool,
    pub cap_runs: Option<u32>,
    pub runs_today: u32,
    pub costs: Vec<TierCost>,
    pub basis: BudgetBasis,
    pub why: String,
}

pub struct BudgetInput<'a> {
    pub now_ms: UnixMs,
    pub account: &'a AccountConfig,
    pub cfg: &'a QuotaConfig,
    /// This account's rows from `quota::views`.
    pub windows: &'a [QuotaView],
    pub owner: OwnerUse,
    pub idlemax_week_pct: f64,
    pub spent_today_pct: f64,
    pub committed_pct: f64,
    pub runs_today: u32,
    /// Spec 2's measuring week (A7); None once it is over.
    pub measuring_left_pct: Option<f64>,
    /// Spec 5: a server-only account.
    pub owner_reserve_override_pct: Option<f64>,
    /// The newest stored limit hit of this account.
    pub limit_hit_ms: Option<UnixMs>,
    pub paused: bool,
    /// The owner's newest use of this provider's CLI.
    pub last_owner_use_ms: Option<UnixMs>,
    /// From `tier_costs`, copied into the Budget.
    pub costs: &'a [TierCost],
}

/// The day's figures before a rule closes the account.
struct Figures {
    slot: DaySlot,
    spendable_pct: f64,
    spent_pct: f64,
    basis: BudgetBasis,
    why: String,
}

fn row(windows: &[QuotaView], kind: QuotaWindowKind) -> Option<&QuotaView> {
    windows.iter().find(|v| v.window == Some(kind))
}

pub fn budget(input: &BudgetInput<'_>) -> Budget {
    let a = input.account;
    let fig = match a.daily_cap_runs {
        Some(cap) => cap_figures(input, cap),
        None => weekly_figures(input),
    };
    let left = (fig.spendable_pct - fig.spent_pct - input.committed_pct).max(0.0);
    let rests_until_ms = input
        .limit_hit_ms
        .and_then(|hit| limit_rest_until(a, input.windows, hit, input.now_ms));
    let closed = closing_rule(input, rests_until_ms, left, &fig);
    let (usable, basis, why) = match closed {
        Some((basis, why)) => (false, basis, why),
        None => (true, fig.basis, fig.why),
    };
    Budget {
        account: a.name.clone(),
        provider: a.provider,
        unit: if a.daily_cap_runs.is_some() {
            BudgetUnit::DailyCapRuns
        } else {
            BudgetUnit::WeeklyPct
        },
        day_start_ms: fig.slot.start_ms,
        day_end_ms: fig.slot.end_ms,
        spendable_pct: fig.spendable_pct,
        spent_pct: fig.spent_pct,
        left_pct: left,
        usable,
        rests_until_ms,
        owner_active: input
            .last_owner_use_ms
            .is_some_and(|t| input.now_ms - t < INTERACTIVE_HOLD_MS),
        cap_runs: a.daily_cap_runs,
        runs_today: input.runs_today,
        costs: input.costs.to_vec(),
        basis,
        why,
    }
}

/// D10.8: the unit is Runs started per UTC day.
fn cap_figures(input: &BudgetInput<'_>, cap: u32) -> Figures {
    let spent = 100.0 * f64::from(input.runs_today) / f64::from(cap.max(1));
    Figures {
        slot: utc_day_slot(input.now_ms),
        spendable_pct: 100.0,
        spent_pct: spent.min(100.0),
        basis: BudgetBasis::DailyCap,
        why: format!("daily cap: {} of {cap} runs used today", input.runs_today),
    }
}

/// D10.2, D10.3, D10.5 and A7 for an account read through its weekly window.
fn weekly_figures(input: &BudgetInput<'_>) -> Figures {
    let now = input.now_ms;
    let weekly = row(input.windows, QuotaWindowKind::Weekly);
    let used = weekly.and_then(|v| v.used_pct).unwrap_or(100.0);
    let spent = input.spent_today_pct;
    let Some(reset) = weekly.and_then(|v| v.resets_at_ms) else {
        return Figures {
            slot: utc_day_slot(now),
            spendable_pct: 0.0,
            spent_pct: spent,
            basis: BudgetBasis::NoData,
            why: "no weekly reading with a reset time".into(),
        };
    };
    let slot = day_slot(reset, now);
    let (mut spendable, mut basis, mut why) = if slot.days_left == 1 {
        let free = (100.0 - used - input.cfg.last_day_floor_pct).max(0.0);
        let why = format!(
            "last day before the reset: {}% used, the floor is {}%",
            num(used),
            num(input.cfg.last_day_floor_pct)
        );
        (free + spent, BudgetBasis::LastDay, why)
    } else {
        paced(input, used, reset, &slot)
    };
    if let Some(m) = input.measuring_left_pct.filter(|m| spent + m < spendable) {
        spendable = spent + m;
        basis = BudgetBasis::Measuring;
        why = format!("measuring week: {}% of the week's cap is left", num(m));
    }
    Figures {
        slot,
        spendable_pct: spendable,
        spent_pct: spent,
        basis,
        why,
    }
}

/// The surplus over the days until the reset, with today's spend added back so the day's
/// figure stays steady while it is spent.
fn paced(
    input: &BudgetInput<'_>,
    used: f64,
    reset: UnixMs,
    slot: &DaySlot,
) -> (f64, BudgetBasis, String) {
    let owner_week = (used - input.idlemax_week_pct).max(0.0);
    let h = hold(
        input.cfg,
        &input.owner,
        owner_week,
        reset,
        input.now_ms,
        input.owner_reserve_override_pct,
    );
    let free = surplus(input.cfg, used, &h);
    let spendable = (free + input.spent_today_pct) / f64::from(slot.days_left.max(1));
    let (basis, held) = if h.reserve {
        (BudgetBasis::Reserve, "the owner's reserve")
    } else {
        (BudgetBasis::Learned, "the owner's usual use")
    };
    let why = format!(
        "{}% used, {held} holds {}%, margin {}%: {}% surplus over {} days",
        num(used),
        num(h.pct),
        num(input.cfg.safety_margin_pct),
        num(free),
        slot.days_left
    );
    (spendable, basis, why)
}

/// The first rule that closes the account, with its reason.
fn closing_rule(
    input: &BudgetInput<'_>,
    rests_until_ms: Option<UnixMs>,
    left: f64,
    fig: &Figures,
) -> Option<(BudgetBasis, String)> {
    let a = input.account;
    let now = input.now_ms;
    let capped = a.daily_cap_runs.is_some();
    if a.provider == Provider::Api {
        return Some((
            BudgetBasis::NotForWorkers,
            "API accounts run no workers".into(),
        ));
    }
    if matches!(a.provider, Provider::Agy | Provider::Opencode) && !capped {
        let why = "set daily_cap_runs for this account in [[quota.accounts]]";
        return Some((BudgetBasis::NoCap, why.into()));
    }
    if input.paused {
        return Some((BudgetBasis::Paused, "paused by you".into()));
    }
    if let Some(until) = rests_until_ms {
        let why = format!(
            "usage limit hit, workers rest for {}",
            duration(until - now)
        );
        return Some((BudgetBasis::Resting, why));
    }
    if !capped {
        if let Some(closed) = reading_rule(input, fig) {
            return Some(closed);
        }
    }
    (left <= 0.0).then(|| {
        let why = format!(
            "today's budget of {}% is spent; the next account day starts in {}",
            num(fig.spendable_pct),
            duration(fig.slot.end_ms - now)
        );
        (BudgetBasis::Spent, why)
    })
}

/// `no_data` and the 5-hour stop (D10.4, A18), for an account paced by its readings.
fn reading_rule(input: &BudgetInput<'_>, fig: &Figures) -> Option<(BudgetBasis, String)> {
    let weekly = row(input.windows, QuotaWindowKind::Weekly);
    let five = row(input.windows, rolling_5h());
    let trusted =
        weekly.is_some_and(|v| v.state == WindowState::Active && v.resets_at_ms.is_some());
    if !trusted || fig.basis == BudgetBasis::NoData {
        return Some((
            BudgetBasis::NoData,
            "no fresh weekly reading with a reset time".into(),
        ));
    }
    if five.is_some_and(|v| v.state == WindowState::Stale) {
        return Some((BudgetBasis::NoData, "5-hour data is older than 6 h".into()));
    }
    let stop = input.cfg.five_hour_stop_pct;
    let used = five
        .filter(|v| v.state == WindowState::Active)
        .and_then(|v| v.used_pct)?;
    (used >= stop).then(|| {
        let why = format!(
            "5h window {}% used, stop line {}%: no worker runs until it resets",
            num(used),
            num(stop)
        );
        (BudgetBasis::FiveHourStop, why)
    })
}

/// 62.0 as "62", 10.67 as "10.7".
fn num(v: f64) -> String {
    let text = format!("{v:.1}");
    text.strip_suffix(".0").map_or(text.clone(), str::to_string)
}
