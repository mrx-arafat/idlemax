//! Adapter: the quota side of the plan and the orchestrator pass (Spec 3): every account's
//! Budget, the accounts a pass may be offered, the cost table, the live `[quota]` and each
//! task's failed checks, read once; and the check of a stored pass's picks against them now.

use idlemax_core::dispatch::orchestrator::QuotaOffer;
use idlemax_core::dispatch::plan::PassResult;
use idlemax_core::dispatch::task::Task;
use idlemax_core::quota::budget::Budget;
use idlemax_core::quota::cost::CostTable;
use idlemax_core::quota::pass::{
    account_offers, check_picks, room_off_clis, AccountOffer, PickFacts, PickRefusal,
};
use idlemax_core::quota::spend::offers;
use idlemax_core::quota::{QuotaConfig, QuotaView};
use idlemax_core::types::UnixMs;
use idlemax_core::Config;

use crate::coord::api::budgets_with_table;
use crate::coord::store::{Store, StoreError};

pub struct QuotaSide {
    pub budgets: Vec<Budget>,
    pub offers: Vec<AccountOffer>,
    pub table: CostTable,
    pub cfg: QuotaConfig,
    /// (task id, Runs whose verdict is `rejected`).
    pub failed: Vec<(String, u32)>,
    /// A usable Budget has room but no CLI in `workers.clis` can spend it.
    pub room_off_clis: bool,
}

impl QuotaSide {
    /// `cfg` gives the accounts, `live` the other `[quota]` keys and `workers.clis` (A2).
    pub fn read(
        store: &Store,
        cfg: &Config,
        live: &Config,
        views: &[QuotaView],
        now: UnixMs,
    ) -> Result<Self, StoreError> {
        let (budgets, table) = budgets_with_table(store, cfg, live, views, now)?;
        let offered = offers(&budgets);
        let room_off_clis = room_off_clis(&offered, &live.workers.clis);
        let offers = account_offers(&offered, &live.workers.clis);
        Ok(Self {
            budgets,
            offers,
            table,
            cfg: live.quota.clone(),
            failed: store.failed_checks()?,
            room_off_clis,
        })
    }

    /// A pick on an account that is not offered says why its Budget is closed.
    fn refusal_text(&self, account: &str, r: &PickRefusal) -> String {
        let closed = self
            .budgets
            .iter()
            .find(|b| b.account == account && !b.usable);
        match (r, closed) {
            (PickRefusal::AccountNotOffered, Some(b)) => {
                format!("account {account} may not be spent: {}", b.why)
            }
            _ => r.to_string(),
        }
    }

    pub fn offer(&self) -> QuotaOffer<'_> {
        QuotaOffer {
            accounts: &self.offers,
            table: &self.table,
            cfg: &self.cfg,
            failed: &self.failed,
        }
    }

    /// The stored pass's picks whose task is still offered, checked again now: a placed pick
    /// takes its tier's model; a refused one is named with the refusal's text, which `build`
    /// gives a slot left empty instead of filling it.
    pub fn recheck(
        &self,
        pass: &PassResult,
        offered: &[&Task],
    ) -> (PassResult, Vec<(String, String)>) {
        let q = self.offer();
        let source = |id: &str| {
            offered
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.source.as_str())
        };
        let live: Vec<_> = pass
            .picks
            .iter()
            .filter(|p| source(&p.task).is_some())
            .collect();
        let facts: Vec<PickFacts<'_>> = live
            .iter()
            .map(|p| PickFacts {
                account: &p.account,
                model: &p.model,
                task_type: source(&p.task).unwrap_or_default(),
                failed_checks: q.failed_checks(&p.task),
            })
            .collect();
        let checked = check_picks(&facts, &self.offers, &self.table, &self.cfg);
        let mut kept = pass.clone();
        kept.picks.retain(|p| source(&p.task).is_none());
        let mut refused = Vec::new();
        for (pick, outcome) in live.into_iter().zip(checked) {
            match outcome {
                Ok(placed) => {
                    let mut pick = pick.clone();
                    if placed.model_changed {
                        pick.reason.push_str(" (model set by the tier rule)");
                    }
                    pick.model = placed.model;
                    kept.picks.push(pick);
                }
                Err(r) => {
                    // It stays among the picks, so `build` meets it and names the refusal.
                    refused.push((pick.task.clone(), self.refusal_text(&pick.account, &r)));
                    kept.picks.push(pick.clone());
                }
            }
        }
        // Keep the pass's order: offered picks first in their order, as `build` walks them.
        let order = |t: &str| pass.picks.iter().position(|p| p.task == t);
        kept.picks.sort_by_key(|p| order(&p.task));
        (kept, refused)
    }
}
