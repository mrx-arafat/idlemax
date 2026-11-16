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

#[tokio::test]
async fn setting_agent_listen_saves_it_without_asking_the_agent_at_the_old_address() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::{http::StatusCode, routing::post, Router};

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::new(root.path().join("state"));
    crate::cli::init::init(&paths, &crate::cli::init::InitOptions::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = crate::state::load_config(&paths).unwrap();
    config.agent.listen = listener.local_addr().unwrap().to_string();
    std::fs::write(paths.config_file(), config.to_toml_string().unwrap()).unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = asked.clone();
    let app = Router::new().route(
        "/v1/local/reload",
        post(move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                StatusCode::UNAUTHORIZED
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let out = Out::new(crate::cli::Format::Toon, false, Vec::new());
    let result = change(&paths, "agent.listen", Some("127.0.0.1:17879"), &out).await;
    server.abort();
    result.unwrap();
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "the agent at the old address must not be asked to reload"
    );
    assert!(std::fs::read_to_string(paths.config_file())
        .unwrap()
        .contains("127.0.0.1:17879"));
}
