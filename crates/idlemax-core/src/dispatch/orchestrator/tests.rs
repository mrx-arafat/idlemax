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
