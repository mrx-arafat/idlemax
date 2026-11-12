//! Domain: decision on when a project's base check runs again when a device's daily budget
//! is spent. Pure.

use crate::quota::budget::{Budget, BudgetBasis};
use crate::types::UnixMs;

pub const CHECK_EVERY_MS: i64 = 86_400_000;

/// True when no account is usable and at least one has basis `Spent` or `FiveHourStop`.
pub fn budget_spent(budgets: &[&Budget]) -> bool {
    if budgets.is_empty() {
        return false;
    }
    if budgets.iter().any(|b| b.usable) {
        return false;
    }
    budgets
        .iter()
        .any(|b| matches!(b.basis, BudgetBasis::Spent | BudgetBasis::FiveHourStop))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckState {
    pub project: i64,
    pub last_ms: Option<UnixMs>,
    pub active: bool,
}

/// Returns project IDs whose check is not active and whose last check was more than
/// `CHECK_EVERY_MS` ago. Empty when `allowed` or `spent` is false.
pub fn checks_due(now_ms: UnixMs, allowed: bool, spent: bool, projects: &[CheckState]) -> Vec<i64> {
    if !allowed || !spent {
        return Vec::new();
    }
    projects
        .iter()
        .filter(|p| !p.active)
        .filter(|p| {
            p.last_ms
                .is_some_and(|last| now_ms.saturating_sub(last) > CHECK_EVERY_MS)
        })
        .map(|p| p.project)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::budget::BudgetUnit;
    use crate::quota::Provider;

    fn test_budget(basis: BudgetBasis, usable: bool) -> Budget {
        Budget {
            account: "test".into(),
            provider: Provider::Claude,
            unit: BudgetUnit::WeeklyPct,
            day_start_ms: 0,
            day_end_ms: 86_400_000,
            spendable_pct: if usable { 10.0 } else { 0.0 },
            spent_pct: 0.0,
            left_pct: if usable { 10.0 } else { 0.0 },
            usable,
            rests_until_ms: None,
            owner_active: false,
            cap_runs: None,
            runs_today: 0,
            costs: Vec::new(),
            basis,
            why: basis.as_str().into(),
        }
    }

    #[test]
    fn budget_spent_decision() {
        let spent = test_budget(BudgetBasis::Spent, false);
        let stop = test_budget(BudgetBasis::FiveHourStop, false);
        let usable = test_budget(BudgetBasis::Learned, true);
        let paused = test_budget(BudgetBasis::Paused, false);

        assert!(budget_spent(&[&spent, &stop]));
        assert!(!budget_spent(&[&spent, &usable]));
        assert!(!budget_spent(&[]));
        assert!(!budget_spent(&[&paused]));
    }

    #[test]
    fn checks_due_decision() {
        let now = 100_000_000;
        let checked_23h = CheckState {
            project: 1,
            last_ms: Some(now - 23 * 3_600_000),
            active: false,
        };
        let active_check = CheckState {
            project: 2,
            last_ms: Some(now - 25 * 3_600_000),
            active: true,
        };
        let checked_25h = CheckState {
            project: 3,
            last_ms: Some(now - 25 * 3_600_000),
            active: false,
        };
        let all = [checked_23h, active_check, checked_25h];

        assert_eq!(checks_due(now, false, true, &all), Vec::<i64>::new());
        assert_eq!(checks_due(now, true, false, &all), Vec::<i64>::new());
        assert_eq!(checks_due(now, true, true, &all), vec![3]);
    }
}
