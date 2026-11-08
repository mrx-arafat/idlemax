//! `idlemax` with no arguments and `idlemax hooks summary` always exit 0 with a short note,
//! whether or not this device is set up and a coordinator answers.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

fn run(home: &Path, state: &Path, args: &[&str]) -> (Output, String) {
    assert!(
        state.starts_with(std::env::temp_dir()),
        "state must be a temp dir"
    );
    let out = Command::new(BIN)
        .args(args)
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env_remove("IDLEMAX_TOKEN")
        .env_remove("IDLEMAX_COORD")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out, text)
}

#[test]
fn home_view_and_hook_summary_explain_a_missing_setup_and_a_missing_coordinator() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, state) = (tmp.path().join("home"), tmp.path().join("state"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    // Not set up: no token yet.
    let (out, text) = run(&home, &state, &[]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("description: "), "{text}");
    assert!(text.contains("status: not_set_up"), "{text}");
    assert!(text.contains("`idlemax setup`"), "{text}");
    let (out, text) = run(&home, &state, &["hooks", "summary"]);
    assert!(out.status.success(), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");

    // Set up, but nothing listens on the coordinator port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = format!("[coordinator]\nurl = \"http://127.0.0.1:{port}\"\n");
    std::fs::write(state.join("config.toml"), config).unwrap();
    let (out, _) = run(&home, &state, &["init"]);
    assert!(out.status.success());
    let (out, text) = run(&home, &state, &[]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("status: unreachable"), "{text}");
    assert!(
        text.contains(&format!("coordinator: \"http://127.0.0.1:{port}\"")),
        "{text}"
    );
    let (out, text) = run(&home, &state, &["hooks", "summary"]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("is not reachable (run `idlemax doctor`)"),
        "{text}"
    );
    let (_, json) = run(&home, &state, &["--json"]);
    assert!(
        json.contains("\"status\": \"down\"") && json.contains("\"reason\": \"unreachable\""),
        "{json}"
    );
}
