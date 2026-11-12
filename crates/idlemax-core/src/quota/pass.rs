//! Domain: what an orchestrator pass may pick on the quota side (D10.4, D10.6, D10.9): the
//! accounts and tier models it is offered, whether a pass is worth starting, the quota part
//! of its input, and whether each pick fits the Budget, the tier and the owner's own use.
//! Pure.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::budget::{Budget, BudgetUnit};
use super::cost::{expected, CostBasis, CostKey, CostTable};
use super::{model_for, model_key, tier_for, Provider, QuotaConfig, Tier, TierPick};

#[cfg(test)]
mod tests;

const SUMMARY_ACCOUNTS: usize = 8;
const NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct TierOffer {
    pub tier: Tier,
    pub model: String,
    pub est_pct: f64,
    pub basis: CostBasis,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountOffer {
    pub account: String,
    pub provider: Provider,
    pub unit: BudgetUnit,
    pub left_pct: f64,
    pub owner_active: bool,
    pub tiers: Vec<TierOffer>,
}

/// One offer per usable Budget whose CLI may run workers and that has a model for a tier.
pub fn account_offers(offered: &[&Budget], worker_clis: &[String]) -> Vec<AccountOffer> {
    offered
        .iter()
        .filter(|b| b.usable)
        .filter(|b| {
            b.provider
                .cli()
                .is_some_and(|cli| worker_clis.iter().any(|c| c == cli))
        })
        .filter_map(|b| {
            let tiers: Vec<TierOffer> = b
                .costs
                .iter()
                .map(|c| TierOffer {
                    tier: c.tier,
                    model: c.model.clone(),
                    est_pct: match b.cap_runs {
                        Some(cap) => 100.0 / f64::from(cap.max(1)),
                        None => c.est_pct,
                    },
                    basis: c.basis,
                })
                .collect();
            (!tiers.is_empty()).then(|| AccountOffer {
                account: b.account.clone(),
                provider: b.provider,
                unit: b.unit,
                left_pct: b.left_pct,
                owner_active: b.owner_active,
                tiers,
            })
        })
        .collect()
}

/// True when an offer has room for one Run of its cheapest tier.
pub fn any_room(offers: &[AccountOffer]) -> bool {
    offers.iter().any(|o| {
        o.tiers
            .iter()
            .map(|t| t.est_pct)
            .reduce(f64::min)
            .is_some_and(|cheapest| cheapest <= o.left_pct)
    })
}

/// True when no offer has room only because no CLI in `worker_clis` can spend the usable
/// Budgets that have room (A21): the reason then names `workers.clis`, not the quota.
pub fn room_off_clis(offered: &[&Budget], worker_clis: &[String]) -> bool {
    let every: Vec<String> = offered
        .iter()
        .filter_map(|b| b.provider.cli())
        .map(String::from)
        .collect();
    !any_room(&account_offers(offered, worker_clis)) && any_room(&account_offers(offered, &every))
}

fn clip(s: &str) -> String {
    s.chars().take(NAME_CHARS).collect()
}

/// The quota part of the pass input: at most 8 accounts and a count of the rest.
pub fn quota_summary(offers: &[AccountOffer]) -> Value {
    let accounts: Vec<Value> = offers
        .iter()
        .take(SUMMARY_ACCOUNTS)
        .map(|o| {
            let tiers: Vec<Value> = o
                .tiers
                .iter()
                .map(|t| json!({"tier": t.tier, "model": clip(&t.model), "est_pct": round(t.est_pct)}))
                .collect();
            json!({"account": clip(&o.account), "provider": o.provider, "unit": o.unit,
                   "left_pct": round(o.left_pct), "owner_active": o.owner_active, "tiers": tiers})
        })
        .collect();
    let more = offers.len().saturating_sub(SUMMARY_ACCOUNTS);
    json!({"accounts": accounts, "more_accounts": more})
}

fn round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// What the rules need to know of one pick.
pub struct PickFacts<'a> {
    pub account: &'a str,
    pub model: &'a str,
    pub task_type: &'a str,
    /// The task's Runs whose verdict is `rejected`.
    pub failed_checks: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub model: String,
    pub tier: Tier,
    pub est_pct: f64,
    /// The orchestrator named another model; the tier's model is used.
    pub model_changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickRefusal {
    AccountNotOffered,
    ModelNotSet { key: &'static str },
    NeedsYou,
    OverBudget { est_pct: f64, left_pct: f64 },
    OwnerActive { free: String },
}

impl PickRefusal {
    /// True when the refusal cannot change before the next pass.
    pub fn is_final(&self) -> bool {
        matches!(
            self,
            PickRefusal::AccountNotOffered
                | PickRefusal::ModelNotSet { .. }
                | PickRefusal::NeedsYou
        )
    }
}

impl std::fmt::Display for PickRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PickRefusal::AccountNotOffered => f.write_str("the account was not offered"),
            PickRefusal::ModelNotSet { key } => {
                write!(f, "no model is set: idlemax config set {key} <model>")
            }
            PickRefusal::NeedsYou => f.write_str("the task failed its checks twice: it needs you"),
            PickRefusal::OverBudget { est_pct, left_pct } => {
                write!(f, "needs {est_pct:.1}%, {left_pct:.1}% left today")
            }
            PickRefusal::OwnerActive { free } => {
                write!(f, "the owner is using this account and {free} has room")
            }
        }
    }
}

/// Checks each pick against its account's offer, in the order given, with the picks on
/// accounts the owner is not using checked first; each placed pick lowers its account's room.
pub fn check_picks(
    picks: &[PickFacts<'_>],
    offers: &[AccountOffer],
    table: &CostTable,
    cfg: &QuotaConfig,
) -> Vec<Result<Placed, PickRefusal>> {
    let mut left: BTreeMap<&str, f64> = offers
        .iter()
        .map(|o| (o.account.as_str(), o.left_pct))
        .collect();
    let busy = |p: &PickFacts<'_>| {
        offers
            .iter()
            .any(|o| o.account == p.account && o.owner_active)
    };
    let mut order: Vec<usize> = (0..picks.len()).collect();
    order.sort_by_key(|i| busy(&picks[*i]));
    let mut out: Vec<Option<Result<Placed, PickRefusal>>> = vec![None; picks.len()];
    for i in order {
        out[i] = Some(check_one(&picks[i], offers, table, cfg, &mut left));
    }
    out.into_iter().flatten().collect()
}

/// The estimate of one Run of `tier` on an offer for this task type.
fn estimate(
    o: &AccountOffer,
    tier: Tier,
    model: &str,
    task_type: &str,
    table: &CostTable,
    cfg: &QuotaConfig,
) -> Option<f64> {
    let offer = o.tiers.iter().find(|t| t.tier == tier)?;
    if o.unit == BudgetUnit::DailyCapRuns {
        return Some(offer.est_pct);
    }
    let key = CostKey {
        provider: o.provider,
        account: o.account.clone(),
        model: model.to_string(),
        task_type: task_type.to_string(),
    };
    Some(expected(table, &key, tier, cfg).pct)
}

fn check_one<'o>(
    p: &PickFacts<'_>,
    offers: &'o [AccountOffer],
    table: &CostTable,
    cfg: &QuotaConfig,
    left: &mut BTreeMap<&'o str, f64>,
) -> Result<Placed, PickRefusal> {
    let offer = offers
        .iter()
        .find(|o| o.account == p.account)
        .ok_or(PickRefusal::AccountNotOffered)?;
    let TierPick::Run(tier) = tier_for(p.task_type, p.failed_checks, cfg) else {
        return Err(PickRefusal::NeedsYou);
    };
    let not_set = || PickRefusal::ModelNotSet {
        key: model_key(offer.provider, tier).unwrap_or("quota.accounts"),
    };
    let model = model_for(offer.provider, tier, cfg)
        .ok_or_else(not_set)?
        .to_string();
    let est = estimate(offer, tier, &model, p.task_type, table, cfg).ok_or_else(not_set)?;
    if offer.owner_active {
        let free = offers.iter().filter(|o| !o.owner_active).find(|o| {
            let room = left.get(o.account.as_str()).copied().unwrap_or(0.0);
            model_for(o.provider, tier, cfg)
                .and_then(|m| estimate(o, tier, m, p.task_type, table, cfg))
                .is_some_and(|e| e <= room)
        });
        if let Some(free) = free {
            return Err(PickRefusal::OwnerActive {
                free: free.account.clone(),
            });
        }
    }
    let room = left.entry(offer.account.as_str()).or_insert(0.0);
    if est > *room {
        return Err(PickRefusal::OverBudget {
            est_pct: est,
            left_pct: *room,
        });
    }
    *room -= est;
    Ok(Placed {
        model_changed: model != p.model,
        model,
        tier,
        est_pct: est,
    })
}
