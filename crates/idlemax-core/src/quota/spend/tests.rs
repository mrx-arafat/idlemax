use super::*;
use crate::quota::budget::tests::{account, view, NOW};
use crate::quota::budget::BudgetUnit;
use crate::quota::{rolling_5h, Provider};
use crate::types::QuotaWindowKind;

const HOUR: i64 = 3_600_000;

fn budget(left_pct: f64, usable: bool, owner_active: bool) -> Budget {
    Budget {
        account: if owner_active { "busy" } else { "idle" }.into(),
        provider: Provider::Claude,
        unit: BudgetUnit::WeeklyPct,
        day_start_ms: NOW,
        day_end_ms: NOW + 24 * HOUR,
        spendable_pct: 10.0,
        spent_pct: 10.0 - left_pct,
        left_pct,
        usable,
        rests_until_ms: None,
        owner_active,
        cap_runs: None,
        runs_today: 0,
        costs: Vec::new(),
        basis: if usable {
            BudgetBasis::Learned
        } else {
            BudgetBasis::Paused
        },
        why: if usable { "ok" } else { "paused by you" }.into(),
    }
}

#[test]
fn the_gate_needs_a_usable_budget_with_room_for_the_estimate() {
    assert!(!gate(None, 3.0).allowed);
    let b = budget(5.3, true, false);
    let refused = gate(Some(&b), 8.0);
    assert_eq!(
        (refused.allowed, refused.reason.as_str()),
        (false, "needs 8.0%, 5.3% left today")
    );
    assert!(gate(Some(&b), 3.0).allowed);
    let closed = gate(Some(&budget(5.3, false, false)), 1.0);
    assert_eq!(
        (closed.allowed, closed.reason.as_str()),
        (false, "paused by you")
    );
}

#[test]
fn only_a_rest_and_the_five_hour_stop_stop_running_workers() {
    use BudgetBasis::*;
    let all = [
        Learned,
        Reserve,
        LastDay,
        Measuring,
        DailyCap,
        Paused,
        Resting,
        NoData,
        NoCap,
        NotForWorkers,
        FiveHourStop,
        Spent,
    ];
    let stopping: Vec<BudgetBasis> = all.into_iter().filter(|b| stops_workers(*b)).collect();
    assert_eq!(stopping, [Resting, FiveHourStop]);
}

#[test]
fn offers_put_the_accounts_the_owner_is_not_using_first() {
    let list = [
        budget(5.0, true, true),
        budget(5.0, true, false),
        budget(5.0, false, false),
    ];
    let names: Vec<&str> = offers(&list).iter().map(|b| b.account.as_str()).collect();
    assert_eq!(names, ["idle", "busy"]);
}

#[test]
fn a_limit_rests_the_account_until_the_used_up_window_resets() {
    let claude = account(Provider::Claude, None);
    let week = |used, reset_in| view(QuotaWindowKind::Weekly, used, reset_in);
    let five = |used, reset_in| view(rolling_5h(), used, reset_in);
    let full_week = [five(40.0, 2 * HOUR), week(97.0, 30 * HOUR)];
    assert_eq!(
        limit_rest_until(&claude, &full_week, NOW, NOW),
        Some(NOW + 30 * HOUR)
    );
    let full_five = [five(100.0, 2 * HOUR), week(50.0, 30 * HOUR)];
    assert_eq!(
        limit_rest_until(&claude, &full_five, NOW, NOW),
        Some(NOW + 2 * HOUR)
    );
    assert_eq!(
        limit_rest_until(&claude, &[], NOW, NOW),
        Some(NOW + 300 * 60_000)
    );
    // Live: only a weekly reading at 51% (the 5-hour window had not started). A limit with no
    // window used up is the 5-hour one, so the rest ends 5 hours on, not at the weekly reset.
    let half_week = [week(51.0, 80 * HOUR)];
    assert_eq!(
        limit_rest_until(&claude, &half_week, NOW, NOW),
        Some(NOW + 5 * HOUR)
    );
    let old_hit = NOW + 30 * HOUR - 7 * 24 * HOUR - HOUR; // before the weekly cycle began
    assert_eq!(limit_rest_until(&claude, &full_five, old_hit, NOW), None);
    assert_eq!(
        rest_event("claude-max", NOW + 3 * HOUR + 10 * 60_000, NOW),
        "claude-max: usage limit hit, workers rest for 3h 10m"
    );
}
