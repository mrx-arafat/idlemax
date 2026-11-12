use super::*;

pub(crate) const NOW: UnixMs = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;

pub(crate) fn view(kind: QuotaWindowKind, used: f64, reset_in: i64) -> QuotaView {
    QuotaView {
        account: "max".into(),
        window: Some(kind),
        state: WindowState::Active,
        used_pct: Some(used),
        resets_at_ms: Some(NOW + reset_in),
        ..QuotaView::default()
    }
}

pub(crate) fn account(provider: Provider, cap: Option<u32>) -> AccountConfig {
    AccountConfig {
        name: "max".into(),
        provider,
        daily_cap_runs: cap,
        ..AccountConfig::default()
    }
}

struct Case {
    account: AccountConfig,
    windows: Vec<QuotaView>,
    owner: OwnerUse,
    idlemax_week_pct: f64,
    spent_today_pct: f64,
    runs_today: u32,
    measuring_left_pct: Option<f64>,
    last_owner_use_ms: Option<UnixMs>,
    limit_hit_ms: Option<UnixMs>,
    committed_pct: f64,
}

impl Case {
    /// Weekly reading 40 resetting in 72 hours, usual use 6 a day over 7 days.
    fn learned() -> Self {
        Self {
            account: account(Provider::Claude, None),
            windows: vec![
                view(rolling_5h(), 10.0, 2 * HOUR),
                view(QuotaWindowKind::Weekly, 40.0, 72 * HOUR),
            ],
            owner: OwnerUse {
                days_known: 7,
                usual_daily_pct: Some(6.0),
            },
            idlemax_week_pct: 0.0,
            spent_today_pct: 0.0,
            runs_today: 0,
            measuring_left_pct: None,
            last_owner_use_ms: None,
            limit_hit_ms: None,
            committed_pct: 0.0,
        }
    }

    fn budget(&self) -> Budget {
        let cfg = QuotaConfig::default();
        budget(&BudgetInput {
            now_ms: NOW,
            account: &self.account,
            cfg: &cfg,
            windows: &self.windows,
            owner: self.owner,
            idlemax_week_pct: self.idlemax_week_pct,
            spent_today_pct: self.spent_today_pct,
            committed_pct: self.committed_pct,
            runs_today: self.runs_today,
            measuring_left_pct: self.measuring_left_pct,
            owner_reserve_override_pct: None,
            limit_hit_ms: self.limit_hit_ms,
            paused: false,
            last_owner_use_ms: self.last_owner_use_ms,
            costs: &[],
        })
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_learned_reserve_last_day_and_measuring_bases() {
    let b = Case::learned().budget();
    assert!(
        close(b.spendable_pct, 32.0 / 3.0) && b.basis == BudgetBasis::Learned,
        "{b:?}"
    );

    let mut reserve = Case::learned();
    reserve.owner = OwnerUse {
        days_known: 3,
        usual_daily_pct: Some(4.0),
    };
    reserve.windows[1] = view(QuotaWindowKind::Weekly, 20.0, 96 * HOUR);
    reserve.idlemax_week_pct = 8.0;
    let b = reserve.budget();
    assert!(
        close(b.spendable_pct, 8.0) && b.basis == BudgetBasis::Reserve,
        "{b:?}"
    );
    assert!(b.why.contains("holds 38%"), "{}", b.why);

    let mut last = Case::learned();
    last.windows[1] = view(QuotaWindowKind::Weekly, 70.0, 20 * HOUR);
    last.spent_today_pct = 5.0;
    let b = last.budget();
    assert_eq!(
        (b.spendable_pct, b.left_pct, b.basis),
        (20.0, 15.0, BudgetBasis::LastDay)
    );

    let mut measuring = Case::learned();
    measuring.measuring_left_pct = Some(4.0);
    let b = measuring.budget();
    assert_eq!((b.spendable_pct, b.basis), (4.0, BudgetBasis::Measuring));
}

#[test]
fn the_five_hour_stop_no_data_and_a_spent_day_close_the_account() {
    let mut five = Case::learned();
    five.windows[0] = view(rolling_5h(), 62.0, 2 * HOUR);
    let b = five.budget();
    assert_eq!((b.usable, b.basis), (false, BudgetBasis::FiveHourStop));
    assert_eq!(
        b.why,
        "5h window 62% used, stop line 60%: no worker runs until it resets"
    );

    let mut hit = Case::learned();
    hit.limit_hit_ms = Some(NOW);
    let b = hit.budget();
    assert_eq!(
        (b.basis, b.rests_until_ms),
        (BudgetBasis::Resting, Some(NOW + 2 * HOUR))
    );

    let mut no_data = Case::learned();
    no_data.windows[1].state = WindowState::NoData;
    assert_eq!(no_data.budget().basis, BudgetBasis::NoData);

    let mut spent = Case::learned();
    spent.spent_today_pct = 32.0 / 3.0 + 1.0;
    spent.idlemax_week_pct = spent.spent_today_pct;
    spent.windows[1].used_pct = Some(40.0 + spent.spent_today_pct);
    let b = spent.budget();
    assert_eq!(b.basis, BudgetBasis::Spent, "{b:?}");
    assert!(
        b.why.contains("the next account day starts in"),
        "{}",
        b.why
    );
}

#[test]
fn a_daily_cap_counts_runs_and_needs_a_cap() {
    let mut cap = Case::learned();
    cap.account = account(Provider::Opencode, Some(5));
    cap.windows.clear();
    cap.runs_today = 3;
    let b = cap.budget();
    assert_eq!(
        (b.spendable_pct, b.spent_pct, b.left_pct, b.usable, b.basis),
        (100.0, 60.0, 40.0, true, BudgetBasis::DailyCap)
    );
    assert_eq!(b.why, "daily cap: 3 of 5 runs used today");
    // A running Run is one of today's started Runs already, and its weekly estimate is in
    // another unit: neither may lower a cap's room again.
    cap.committed_pct = 20.0;
    assert_eq!(cap.budget().left_pct, 40.0);
    cap.runs_today = 5;
    assert_eq!(cap.budget().basis, BudgetBasis::Spent);
    cap.account = account(Provider::Opencode, None);
    assert_eq!(cap.budget().basis, BudgetBasis::NoCap);
}

#[test]
fn the_owner_is_active_for_thirty_minutes_after_a_use() {
    let mut c = Case::learned();
    c.last_owner_use_ms = Some(NOW - 10 * 60_000);
    assert!(c.budget().owner_active);
    c.last_owner_use_ms = Some(NOW - 40 * 60_000);
    assert!(!c.budget().owner_active);
}
