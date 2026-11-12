//! Domain: the cost table of D10.1: what percent of each window one worker Run costs, per
//! provider, account, model and task type, measured from the readings around Runs and
//! seeded by the tier until a key has enough samples. Pure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    model_for, window_minutes, Provider, QuotaConfig, Tier, SAME_RESET_MS, SNAPSHOT_MAX_AGE_MS,
};
use crate::types::{QuotaSnapshot, QuotaWindowKind, UnixMs};

#[cfg(test)]
mod tests;

/// Samples a key needs before its mean replaces the seed.
pub const MIN_SAMPLES: usize = 5;
/// Newest measured costs kept per key and window.
const KEEP_SAMPLES: usize = 20;

/// One worker Run as the cost table sees it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RunSample {
    pub provider: Provider,
    pub account: String,
    pub model: String,
    pub task_type: String,
    pub started_ms: UnixMs,
    /// None while the Run is active.
    pub ended_ms: Option<UnixMs>,
    /// Jobs started for the Run.
    pub launches: u32,
    /// At most one reading per window.
    pub before: Vec<QuotaSnapshot>,
    pub after: Vec<QuotaSnapshot>,
}

/// What one Run cost on one window; None when it cannot be measured.
#[derive(Debug, Clone, PartialEq)]
pub struct RunCost {
    pub sample: usize,
    pub window: QuotaWindowKind,
    pub pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CostKey {
    pub provider: Provider,
    pub account: String,
    pub model: String,
    pub task_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostBasis {
    Measured,
    SameModel,
    Seed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub pct: f64,
    pub basis: CostBasis,
    pub samples: u32,
}

/// One tier of one account as the owner sees it: the estimate over every task type of the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierCost {
    pub tier: Tier,
    pub model: String,
    pub est_pct: f64,
    pub basis: CostBasis,
    pub samples: u32,
    pub five_hour_pct: Option<f64>,
}

/// A map from `CostKey` and window to the newest 20 measured costs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostTable {
    /// (key, window minutes) to costs, newest last.
    rows: BTreeMap<(CostKey, u32), Vec<f64>>,
}

fn reading(list: &[QuotaSnapshot], window: QuotaWindowKind) -> Option<&QuotaSnapshot> {
    list.iter().find(|s| s.window == window)
}

/// The before and after readings of a Run on a window, when they measure it.
fn readings(s: &RunSample, window: QuotaWindowKind) -> Option<(f64, f64)> {
    let end = s.ended_ms?;
    if s.launches != 1 {
        return None; // a Run shed in between is never a sample
    }
    let before = reading(&s.before, window)?;
    let after = reading(&s.after, window)?;
    let fresh = before.at_ms <= s.started_ms && s.started_ms - before.at_ms <= SNAPSHOT_MAX_AGE_MS;
    let same_cycle = match (before.resets_at_ms, after.resets_at_ms) {
        (Some(a), Some(b)) => (a - b).abs() <= SAME_RESET_MS,
        _ => false,
    };
    (fresh && after.at_ms >= end && same_cycle).then_some((before.used_pct, after.used_pct))
}

/// Each Run's cost per window. Overlapping Runs on one account share the reading: time is cut
/// at every start and end, and the rise across a segment is split equally among the Runs
/// active in it; a fall counts as 0.
pub fn run_costs(samples: &[RunSample]) -> Vec<RunCost> {
    let mut windows: Vec<QuotaWindowKind> = Vec::new();
    for w in samples
        .iter()
        .flat_map(|s| s.before.iter().chain(&s.after))
        .map(|r| r.window)
    {
        if !windows.contains(&w) {
            windows.push(w);
        }
    }
    let mut out = Vec::new();
    for window in windows {
        let measured = window_costs(samples, window);
        for (i, pct) in measured.into_iter().enumerate() {
            out.push(RunCost {
                sample: i,
                window,
                pct,
            });
        }
    }
    out
}

/// Costs on one window, by sample index.
fn window_costs(samples: &[RunSample], window: QuotaWindowKind) -> Vec<Option<f64>> {
    let mut costs: Vec<Option<f64>> = vec![None; samples.len()];
    let mut accounts: Vec<&str> = samples.iter().map(|s| s.account.as_str()).collect();
    accounts.sort_unstable();
    accounts.dedup();
    for account in accounts {
        // (index, start, end, before, after) of every measurable Run of the account
        let runs: Vec<(usize, UnixMs, UnixMs, f64, f64)> = samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.account == account)
            .filter_map(|(i, s)| {
                let (b, a) = readings(s, window)?;
                Some((i, s.started_ms, s.ended_ms?, b, a))
            })
            .collect();
        let mut cuts: Vec<(UnixMs, f64)> = Vec::new();
        for &(_, start, end, b, a) in &runs {
            for (t, v) in [(start, b), (end, a)] {
                if !cuts.iter().any(|(ct, _)| *ct == t) {
                    cuts.push((t, v));
                }
            }
        }
        cuts.sort_by_key(|(t, _)| *t);
        for &(i, ..) in &runs {
            costs[i] = Some(0.0);
        }
        for pair in cuts.windows(2) {
            let ((t0, v0), (t1, v1)) = (pair[0], pair[1]);
            let active: Vec<usize> = runs
                .iter()
                .filter(|(_, s, e, ..)| *s <= t0 && *e >= t1)
                .map(|(i, ..)| *i)
                .collect();
            if active.is_empty() {
                continue;
            }
            let share = (v1 - v0).max(0.0) / active.len() as f64;
            for i in active {
                costs[i] = costs[i].map(|c| c + share);
            }
        }
    }
    costs
}

impl CostKey {
    fn of(s: &RunSample) -> Self {
        Self {
            provider: s.provider,
            account: s.account.clone(),
            model: s.model.clone(),
            task_type: s.task_type.clone(),
        }
    }
}

/// The table: the newest 20 measured costs per key and window, by the Run's end.
pub fn cost_table(samples: &[RunSample], costs: &[RunCost]) -> CostTable {
    let mut measured: Vec<(&RunSample, &RunCost, f64)> = costs
        .iter()
        .filter_map(|c| Some((samples.get(c.sample)?, c, c.pct?)))
        .collect();
    measured.sort_by_key(|(s, ..)| s.ended_ms);
    let mut table = CostTable::default();
    for (s, c, pct) in measured {
        let row = table
            .rows
            .entry((CostKey::of(s), window_minutes(c.window)))
            .or_default();
        row.push(pct);
        if row.len() > KEEP_SAMPLES {
            row.remove(0);
        }
    }
    table
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

impl CostTable {
    /// Every cost of one model on one window, over its task types.
    fn model_costs(&self, key: &CostKey, minutes: u32) -> Vec<f64> {
        self.rows
            .iter()
            .filter(|((k, m), _)| {
                *m == minutes
                    && k.provider == key.provider
                    && k.account == key.account
                    && k.model == key.model
            })
            .flat_map(|(_, v)| v.iter().copied())
            .collect()
    }
}

fn seed(tier: Tier, cfg: &QuotaConfig) -> f64 {
    match tier {
        Tier::Small => cfg.seed_small_pct,
        Tier::Standard => cfg.seed_standard_pct,
    }
}

const WEEKLY_MINUTES: u32 = super::WEEK_MINUTES;

/// What one Run of this key is expected to cost, in percent of the weekly window.
pub fn expected(table: &CostTable, key: &CostKey, tier: Tier, cfg: &QuotaConfig) -> CostEstimate {
    let estimate = |values: &[f64], basis| CostEstimate {
        pct: mean(values),
        basis,
        samples: u32::try_from(values.len()).unwrap_or(u32::MAX),
    };
    if let Some(own) = table.rows.get(&(key.clone(), WEEKLY_MINUTES)) {
        if own.len() >= MIN_SAMPLES {
            return estimate(own, CostBasis::Measured);
        }
    }
    let same = table.model_costs(key, WEEKLY_MINUTES);
    if same.len() >= MIN_SAMPLES {
        return estimate(&same, CostBasis::SameModel);
    }
    CostEstimate {
        pct: seed(tier, cfg),
        basis: CostBasis::Seed,
        samples: u32::try_from(same.len()).unwrap_or(u32::MAX),
    }
}

/// One line per tier whose model key is set.
pub fn tier_costs(
    provider: Provider,
    account: &str,
    table: &CostTable,
    cfg: &QuotaConfig,
) -> Vec<TierCost> {
    [Tier::Small, Tier::Standard]
        .into_iter()
        .filter_map(|tier| {
            let model = model_for(provider, tier, cfg)?.to_string();
            let key = CostKey {
                provider,
                account: account.to_string(),
                model: model.clone(),
                task_type: String::new(),
            };
            let weekly = table.model_costs(&key, WEEKLY_MINUTES);
            let five = table.model_costs(&key, window_minutes(super::rolling_5h()));
            let measured = weekly.len() >= MIN_SAMPLES;
            Some(TierCost {
                tier,
                model,
                est_pct: if measured {
                    mean(&weekly)
                } else {
                    seed(tier, cfg)
                },
                basis: if measured {
                    CostBasis::Measured
                } else {
                    CostBasis::Seed
                },
                samples: u32::try_from(weekly.len()).unwrap_or(u32::MAX),
                five_hour_pct: (five.len() >= MIN_SAMPLES).then(|| mean(&five)),
            })
        })
        .collect()
}
