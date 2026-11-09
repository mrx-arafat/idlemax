//! Tests: the wording of what `idlemax config set` and `unset` report.

use serde_json::json;

use super::*;

fn edit(before: Value, after: Value) -> Edit {
    Edit {
        text: String::new(),
        changed: before != after,
        before,
        after,
    }
}

#[test]
fn the_result_says_what_changed_and_what_the_agent_did_about_it() {
    let key = "guard.require_ac_power";
    let e = edit(json!(true), json!(false));
    assert_eq!(
        headline(key, &e, false, true),
        "guard.require_ac_power: true -> false"
    );
    assert_eq!(
        headline(key, &edit(json!(false), json!(false)), false, false),
        "guard.require_ac_power is already false (no-op)"
    );
    assert_eq!(
        headline(key, &edit(json!(false), json!(true)), true, true),
        "guard.require_ac_power: false -> true (default restored)"
    );
    assert!(headline(key, &edit(json!(true), json!(true)), true, true)
        .contains("removed from config.toml"));

    let report = |applied: &[&str], restart: &[&str]| ReloadReport {
        applied: applied.iter().map(|s| s.to_string()).collect(),
        restart_agent: restart.iter().map(|s| s.to_string()).collect(),
        restart_coordinator: Vec::new(),
    };
    let note = |r| agent_note(key, &Agent::Reloaded(r)).unwrap();
    assert_eq!(note(report(&[key], &[])), "applied now, no restart needed");
    assert!(note(report(&[], &[key])).contains("restart the agent"));
    assert!(agent_note(key, &Agent::NotRunning)
        .unwrap()
        .contains("not running"));
    assert_eq!(agent_note(key, &Agent::Unchanged), None);
}
