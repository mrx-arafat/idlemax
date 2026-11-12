//! Delivery: the budget table of `idlemax quota` and its TOON list: per account today's
//! Budget, what was spent, what is left, whether workers may run and why (Spec 3).

use idlemax_core::quota::budget::{Budget, BudgetUnit};
use idlemax_core::quota::QuotaView;
use idlemax_core::types::UnixMs;

use super::toon::Node;
use crate::cli::print::table;
use crate::cli::rows::rel;

pub const HEADER: [&str; 7] = [
    "ACCOUNT", "BUDGET", "SPENT", "LEFT", "WORKERS", "DAY ENDS", "WHY",
];

/// Each account's Budget once, in the order of the rows; none from a coordinator that
/// predates it.
pub fn budgets(views: &[QuotaView]) -> Vec<&Budget> {
    let mut out: Vec<&Budget> = Vec::new();
    for b in views.iter().filter_map(|v| v.budget.as_ref()) {
        if !out.iter().any(|o| o.account == b.account) {
            out.push(b);
        }
    }
    out
}

/// (budget, spent, left) as people read them: percent of the weekly window, or Runs on a cap.
fn amounts(b: &Budget) -> [String; 3] {
    match (b.unit, b.cap_runs) {
        (BudgetUnit::DailyCapRuns, Some(cap)) => {
            let left = cap.saturating_sub(b.runs_today);
            [cap, b.runs_today, left].map(|n| format!("{n} runs"))
        }
        _ => [b.spendable_pct, b.spent_pct, b.left_pct].map(|p| format!("{p:.1}%")),
    }
}

pub fn cells(b: &Budget, now_ms: UnixMs) -> Vec<String> {
    let [budget, spent, left] = amounts(b);
    vec![
        b.account.clone(),
        budget,
        spent,
        left,
        if b.usable { "yes" } else { "no" }.into(),
        rel(b.day_end_ms, now_ms),
        b.why.clone(),
    ]
}

/// The budget table, or None when no row carries a Budget.
pub fn budget_table(views: &[QuotaView], now_ms: UnixMs) -> Option<String> {
    let rows: Vec<Vec<String>> = budgets(views).iter().map(|b| cells(b, now_ms)).collect();
    (!rows.is_empty()).then(|| table(&HEADER, &rows))
}

/// The windows list with a `budgets` list next to it, as `views::status` joins devices and
/// accounts; the windows list alone when no row carries a Budget.
pub fn with_budgets(windows: Node, views: &[QuotaView], now_ms: UnixMs) -> Node {
    let list = budgets(views);
    let (Node::Obj(mut fields), false) = (windows.clone(), list.is_empty()) else {
        return windows;
    };
    let rows = list
        .iter()
        .map(|b| {
            let [budget, spent, left] = amounts(b);
            Node::obj([
                ("account", Node::str(&b.account)),
                ("budget", Node::str(budget)),
                ("spent", Node::str(spent)),
                ("left", Node::str(left)),
                ("workers", Node::Bool(b.usable)),
                ("day_ends", Node::str(rel(b.day_end_ms, now_ms))),
                ("basis", Node::str(b.basis.as_str())),
                ("why", Node::str(&b.why)),
            ])
        })
        .collect();
    fields.push(("budgets".into(), Node::Arr(rows)));
    Node::Obj(fields)
}
