//! Domain: which AI accounts the dispatcher may use now, as the plan and the orchestrator pass
//! see them. Pure. Each account is usable when its Budget is (Spec 3), and carries the
//! Budget's reason; the two used percents come from the quota views.

use serde::{Deserialize, Serialize};

use crate::quota::budget::Budget;
use crate::quota::{rolling_5h, Provider, QuotaView, WindowState};
use crate::types::QuotaWindowKind;

/// The measuring week's cap of A7, in percent of the weekly window.
pub const MEASURING_WEEK_CAP_PCT: f64 = 10.0;

/// One account as the dispatcher sees it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountUse {
    pub account: String,
    pub provider: Provider,
    pub usable: bool,
    /// Why it is or is not usable.
    pub why: String,
    pub five_hour_used_pct: Option<f64>,
    pub weekly_used_pct: Option<f64>,
}

/// One entry per Budget, in the Budgets' order.
pub fn account_uses(budgets: &[Budget], views: &[QuotaView]) -> Vec<AccountUse> {
    budgets
        .iter()
        .map(|b| {
            let row = |kind: QuotaWindowKind| {
                views
                    .iter()
                    .find(|v| v.account == b.account && v.window == Some(kind))
            };
            let five = row(rolling_5h());
            AccountUse {
                account: b.account.clone(),
                provider: b.provider,
                usable: b.usable,
                why: b.why.clone(),
                five_hour_used_pct: match five.map(|v| v.state) {
                    None | Some(WindowState::NotStarted) => Some(0.0),
                    _ => five.and_then(|v| v.used_pct),
                },
                weekly_used_pct: row(QuotaWindowKind::Weekly).and_then(|v| v.used_pct),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::budget::{BudgetBasis, BudgetUnit};

    #[test]
    fn an_account_is_usable_when_its_budget_is() {
        let why = "5h window 62% used, stop line 60%: no worker runs until it resets";
        let b = Budget {
            account: "max".into(),
            provider: Provider::Claude,
            unit: BudgetUnit::WeeklyPct,
            day_start_ms: 0,
            day_end_ms: 0,
            spendable_pct: 5.0,
            spent_pct: 0.0,
            left_pct: 5.0,
            usable: false,
            rests_until_ms: None,
            owner_active: false,
            cap_runs: None,
            runs_today: 0,
            costs: Vec::new(),
            basis: BudgetBasis::FiveHourStop,
            why: why.into(),
        };
        let five = QuotaView {
            account: "max".into(),
            window: Some(rolling_5h()),
            state: WindowState::Active,
            used_pct: Some(62.0),
            ..QuotaView::default()
        };
        let uses = account_uses(&[b], &[five]);
        assert_eq!(
            (
                uses[0].usable,
                uses[0].why.as_str(),
                uses[0].five_hour_used_pct
            ),
            (false, why, Some(62.0))
        );
    }
}
