use super::*;
use crate::quota::cost::TierCost;

fn cfg() -> QuotaConfig {
    QuotaConfig {
        codex_small_model: Some("mini".into()),
        codex_standard_model: Some("gpt".into()),
        ..QuotaConfig::default()
    }
}

fn offer(account: &str, provider: Provider, left_pct: f64, owner_active: bool) -> AccountOffer {
    let c = cfg();
    let tiers = [Tier::Small, Tier::Standard]
        .into_iter()
        .filter_map(|tier| {
            Some(TierOffer {
                tier,
                model: model_for(provider, tier, &c)?.to_string(),
                est_pct: if tier == Tier::Small { 3.0 } else { 8.0 },
                basis: CostBasis::Seed,
            })
        })
        .collect();
    AccountOffer {
        account: account.into(),
        provider,
        unit: BudgetUnit::WeeklyPct,
        left_pct,
        owner_active,
        tiers,
    }
}

fn pick<'a>(account: &'a str, model: &'a str, task_type: &'a str, failed: u32) -> PickFacts<'a> {
    PickFacts {
        account,
        model,
        task_type,
        failed_checks: failed,
    }
}

fn check(picks: &[PickFacts<'_>], offers: &[AccountOffer]) -> Vec<Result<Placed, PickRefusal>> {
    check_picks(picks, offers, &CostTable::default(), &cfg())
}

#[test]
fn the_tier_sets_the_model_and_two_failures_need_the_owner() {
    let offers = [offer("max", Provider::Claude, 50.0, false)];
    let placed = |r: &Result<Placed, PickRefusal>| r.clone().unwrap();
    let r = check(&[pick("max", "haiku", "broken_check", 0)], &offers);
    assert_eq!(
        (placed(&r[0]).model.as_str(), placed(&r[0]).model_changed),
        ("haiku", false)
    );
    let r = check(&[pick("max", "sonnet", "broken_check", 0)], &offers);
    assert_eq!(
        (placed(&r[0]).model.as_str(), placed(&r[0]).model_changed),
        ("haiku", true)
    );
    let r = check(&[pick("max", "haiku", "broken_check", 1)], &offers);
    assert_eq!(placed(&r[0]).model, "sonnet");
    let r = check(&[pick("max", "sonnet", "broken_check", 2)], &offers);
    assert_eq!(r[0], Err(PickRefusal::NeedsYou));
}

#[test]
fn picks_cannot_overdraw_a_day_or_use_what_was_not_offered() {
    let mut offers = vec![offer("max", Provider::Claude, 6.0, false)];
    offers[0].tiers[1].est_pct = 4.0;
    let mut table_cfg = cfg();
    table_cfg.seed_standard_pct = 4.0;
    let two = [
        pick("max", "sonnet", "roadmap", 0),
        pick("max", "sonnet", "roadmap", 0),
    ];
    let r = check_picks(&two, &offers, &CostTable::default(), &table_cfg);
    assert!(r[0].is_ok());
    assert!(matches!(r[1], Err(PickRefusal::OverBudget { .. })), "{r:?}");

    let r = check(&[pick("other", "sonnet", "roadmap", 0)], &offers);
    assert_eq!(r[0], Err(PickRefusal::AccountNotOffered));
    let agy = [offer("agy1", Provider::Agy, 50.0, false)];
    let r = check(&[pick("agy1", "x", "roadmap", 0)], &agy);
    assert_eq!(
        r[0],
        Err(PickRefusal::ModelNotSet {
            key: "quota.agy_standard_model"
        })
    );
}

#[test]
fn an_account_in_use_yields_to_a_free_one_with_room() {
    let busy = |codex_left| {
        vec![
            offer("codex", Provider::Codex, codex_left, false),
            offer("claude", Provider::Claude, 12.0, true),
        ]
    };
    let p = [pick("claude", "haiku", "broken_check", 0)];
    assert!(check(&p, &busy(1.0))[0].is_ok());
    assert_eq!(
        check(&p, &busy(10.0))[0],
        Err(PickRefusal::OwnerActive {
            free: "codex".into()
        })
    );
    let both = [
        pick("claude", "haiku", "broken_check", 0),
        pick("codex", "mini", "broken_check", 0),
    ];
    let r = check(&both, &busy(3.0));
    assert!(r[0].is_ok() && r[1].is_ok(), "{r:?}");
}

#[test]
fn only_refusals_that_cannot_change_are_final() {
    assert!(PickRefusal::AccountNotOffered.is_final());
    assert!(PickRefusal::ModelNotSet { key: "k" }.is_final());
    assert!(PickRefusal::NeedsYou.is_final());
    let over = PickRefusal::OverBudget {
        est_pct: 1.0,
        left_pct: 0.5,
    };
    assert!(!over.is_final());
    assert!(!PickRefusal::OwnerActive { free: "x".into() }.is_final());
}

fn budget(account: &str, provider: Provider, left_pct: f64) -> Budget {
    Budget {
        account: account.into(),
        provider,
        unit: BudgetUnit::WeeklyPct,
        day_start_ms: 0,
        day_end_ms: 0,
        spendable_pct: 10.0,
        spent_pct: 0.0,
        left_pct,
        usable: true,
        rests_until_ms: None,
        owner_active: false,
        cap_runs: Some(5),
        runs_today: 0,
        costs: vec![TierCost {
            tier: Tier::Standard,
            model: "m".into(),
            est_pct: 8.0,
            basis: CostBasis::Seed,
            samples: 0,
            five_hour_pct: None,
        }],
        basis: crate::quota::budget::BudgetBasis::Learned,
        why: String::new(),
    }
}

#[test]
fn offers_follow_the_worker_clis_and_room_needs_the_cheapest_tier() {
    let (claude, agy) = (
        budget("max", Provider::Claude, 50.0),
        budget("agy1", Provider::Agy, 50.0),
    );
    let offers = account_offers(&[&claude, &agy], &["claude".to_string()]);
    let names: Vec<&str> = offers.iter().map(|o| o.account.as_str()).collect();
    assert_eq!(names, ["max"]);
    assert!(!any_room(&[]));
    assert!(!any_room(&[offer("max", Provider::Claude, 2.0, false)]));
    assert!(any_room(&[offer("max", Provider::Claude, 3.0, false)]));
    assert!(room_off_clis(&[&claude, &agy], &[]));
    assert!(!room_off_clis(&[&claude], &["claude".to_string()]));
}

#[test]
fn the_quota_summary_stays_short() {
    let offers: Vec<AccountOffer> = (0..40)
        .map(|i| {
            offer(
                &format!("{}-{i}", "a".repeat(80)),
                Provider::Claude,
                9.0,
                false,
            )
        })
        .collect();
    let summary = quota_summary(&offers);
    assert!(
        summary.to_string().len() < 4096,
        "{}",
        summary.to_string().len()
    );
    assert_eq!(summary["accounts"].as_array().unwrap().len(), 8);
    assert_eq!(summary["more_accounts"], 32);
}
