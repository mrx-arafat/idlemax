use super::*;
use crate::dispatch::room::RoomDecision;
use crate::dispatch::task::TaskSource;
use crate::types::DeviceInfo;

const NOW: UnixMs = 1_800_000_000_000;

fn task(id: &str) -> Task {
    Task {
        id: id.into(),
        project: "app".into(),
        project_id: 1,
        device: "mac".into(),
        source: TaskSource::Roadmap,
        title: format!("Do {id}"),
        done_check: Some("it works".into()),
        scope_paths: Vec::new(),
        needs_you: None,
    }
}

fn account(name: &str, provider: Provider, usable: bool) -> AccountUse {
    AccountUse {
        account: name.into(),
        provider,
        usable,
        ..Default::default()
    }
}

fn models() -> Vec<(Provider, String)> {
    vec![
        (Provider::Claude, "sonnet".into()),
        (Provider::Codex, "gpt".into()),
    ]
}

fn out(picks: Value) -> Value {
    json!({"picks": picks, "needs_you": []})
}

fn pick(task: &str, account: &str, model: &str, scope: &[&str]) -> Value {
    json!({"task": task, "account": account, "model": model, "reason": "small", "scope_paths": scope})
}

#[test]
fn validate_keeps_offered_picks_and_drops_the_rest_by_name() {
    let tasks = [task("a"), task("b")];
    let refs: Vec<&Task> = tasks.iter().collect();
    let accounts = [account("max", Provider::Claude, true)];
    let models = models();
    let offer = Offer {
        tasks: &refs,
        accounts: &accounts,
        models: &models,
        quota: None,
    };
    let output = out(json!([
        pick("a", "max", "sonnet", &["src/a.rs", "tests"]),
        pick("zzz", "max", "sonnet", &[]),
        pick("b", "other", "sonnet", &[]),
        pick("b", "max", "gpt", &[]),
        pick("b", "max", "sonnet", &["../x"]),
    ]));
    let result = validate(&output, &offer).unwrap();
    assert_eq!(result.picks.len(), 1);
    assert_eq!(result.picks[0].scope_paths, ["src/a.rs", "tests"]);
    assert_eq!(result.dropped.len(), 4, "{:?}", result.dropped);
    assert!(result.seen.is_empty());

    let mut extra = pick("a", "max", "sonnet", &[]);
    extra["command"] = json!("rm -rf /");
    assert!(validate(&out(json!([extra])), &offer).is_err());
    let nine: Vec<Value> = (0..9).map(|_| pick("a", "max", "sonnet", &[])).collect();
    assert!(validate(&out(json!(nine)), &offer).is_err());

    let both = json!({"picks": [pick("a", "max", "sonnet", &[])],
                      "needs_you": [{"task": "a", "reason": "vague"}]});
    let result = validate(&both, &offer).unwrap();
    assert!(result.picks.is_empty());
    assert_eq!(result.needs_you, [("a".to_string(), "vague".to_string())]);
}

#[test]
fn the_fingerprint_ignores_order_and_follows_every_input() {
    let print = |tasks: &[(&str, &str)], rules: &str| {
        let tasks: Vec<(String, String)> = tasks
            .iter()
            .map(|(i, d)| (i.to_string(), d.to_string()))
            .collect();
        fingerprint(&FingerprintParts {
            devices: &["mac".into(), "box".into()],
            tasks: &tasks,
            accounts: &["max".into()],
            models: &models(),
            rules,
        })
    };
    let base = print(&[("a", "1"), ("b", "2")], "rules");
    assert_eq!(base, print(&[("b", "2"), ("a", "1")], "rules"));
    assert_ne!(base, print(&[("a", "1"), ("b", "2"), ("c", "3")], "rules"));
    assert_ne!(base, print(&[("a", "1"), ("b", "2")], "rules\nmore"));
    assert_ne!(base, print(&[("a", "1"), ("b", "9")], "rules"));
}

fn device(state: LineState) -> DeviceView {
    DeviceView {
        info: DeviceInfo {
            id: "mac".into(),
            capabilities: vec!["claude".into()],
            ai_accounts: vec!["max".into()],
            ..Default::default()
        },
        online: true,
        room: Some(RoomDecision {
            state,
            open_slots: 2,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn a_pass_is_due_only_when_the_rules_allow_and_something_changed() {
    let tasks = [task("a")];
    let refs: Vec<&Task> = tasks.iter().collect();
    let usable = [account("max", Provider::Claude, true)];
    let spent = [account("max", Provider::Claude, false)];
    let models = models();
    let offer = |accounts| Offer {
        tasks: &refs,
        accounts,
        models: &models,
        quota: None,
    };
    let (open, held) = ([device(LineState::Run)], [device(LineState::Hold)]);
    let ok_offer = offer(&usable);
    let base = DueInput {
        now_ms: NOW,
        mode: Mode::Propose,
        asked: false,
        devices: &open,
        paused: &[],
        offer: &ok_offer,
        fingerprint: "new",
        newest: None,
        pass_active: false,
        room: true,
    };
    assert_eq!(
        pass_due(&base),
        Ok(DuePass {
            account: "max".into(),
            device: "mac".into()
        })
    );
    assert!(pass_due(&DueInput {
        mode: Mode::Off,
        ..base
    })
    .is_err());
    assert!(pass_due(&DueInput {
        devices: &held,
        ..base
    })
    .is_err());
    let spent_offer = offer(&spent);
    assert!(pass_due(&DueInput {
        offer: &spent_offer,
        ..base
    })
    .is_err());
    assert_eq!(
        pass_due(&DueInput {
            pass_active: true,
            ..base
        }),
        Err(PASS_ACTIVE.into())
    );
    let recent = DueInput {
        newest: Some(("old", NOW - 60_000)),
        ..base
    };
    assert!(pass_due(&recent).is_err());
    assert!(pass_due(&DueInput {
        asked: true,
        ..recent
    })
    .is_ok());
}

#[test]
fn validate_places_each_pick_on_its_tier_and_keeps_one_that_may_fit_later() {
    use crate::quota::budget::BudgetUnit;
    use crate::quota::cost::{CostBasis, CostTable};
    use crate::quota::pass::{AccountOffer, TierOffer};
    use crate::quota::{QuotaConfig, Tier};
    let mut fix = task("fix");
    fix.source = TaskSource::BrokenCheck;
    let tasks = [fix, task("big")];
    let refs: Vec<&Task> = tasks.iter().collect();
    let accounts = [
        account("max", Provider::Claude, true),
        account("cx", Provider::Codex, true),
    ];
    let tier = |tier, model: &str| TierOffer {
        tier,
        model: model.into(),
        est_pct: 0.0,
        basis: CostBasis::Seed,
    };
    let offer_of = |name: &str, provider, left_pct, tiers| AccountOffer {
        account: name.into(),
        provider,
        unit: BudgetUnit::WeeklyPct,
        left_pct,
        owner_active: false,
        tiers,
    };
    let account_offers = [
        offer_of(
            "max",
            Provider::Claude,
            5.0,
            vec![tier(Tier::Small, "haiku"), tier(Tier::Standard, "sonnet")],
        ),
        offer_of(
            "cx",
            Provider::Codex,
            50.0,
            vec![tier(Tier::Standard, "gpt")],
        ),
    ];
    let cfg = QuotaConfig {
        codex_standard_model: Some("gpt".into()),
        ..QuotaConfig::default()
    };
    let table = CostTable::default();
    let quota = QuotaOffer {
        accounts: &account_offers,
        table: &table,
        cfg: &cfg,
        failed: &[],
    };
    let models = vec![
        (Provider::Claude, "haiku".to_string()),
        (Provider::Claude, "sonnet".to_string()),
        (Provider::Codex, "gpt".to_string()),
    ];
    let offer = Offer {
        tasks: &refs,
        accounts: &accounts,
        models: &models,
        quota: Some(&quota),
    };
    let output = out(json!([
        pick("fix", "max", "sonnet", &[]),
        pick("big", "max", "sonnet", &[]),
    ]));
    let result = validate(&output, &offer).unwrap();
    let placed: Vec<(&str, &str)> = result
        .picks
        .iter()
        .map(|p| (p.task.as_str(), p.model.as_str()))
        .collect();
    assert_eq!(placed, [("fix", "haiku"), ("big", "sonnet")], "{result:?}");

    let on_codex = out(json!([pick("fix", "cx", "gpt", &[])]));
    let result = validate(&on_codex, &offer).unwrap();
    assert!(result.picks.is_empty());
    assert!(
        result.dropped[0].contains("quota.codex_small_model"),
        "{:?}",
        result.dropped
    );
}

#[test]
fn no_pass_runs_when_no_account_has_room() {
    let tasks = [task("a")];
    let refs: Vec<&Task> = tasks.iter().collect();
    let usable = [account("max", Provider::Claude, true)];
    let models = models();
    let offer = Offer {
        tasks: &refs,
        accounts: &usable,
        models: &models,
        quota: None,
    };
    let open = [device(LineState::Run)];
    let input = DueInput {
        now_ms: NOW,
        mode: Mode::Propose,
        asked: true,
        devices: &open,
        paused: &[],
        offer: &offer,
        fingerprint: "x",
        newest: None,
        pass_active: false,
        room: false,
    };
    assert_eq!(pass_due(&input), Err(NO_ROOM.into()));
}
