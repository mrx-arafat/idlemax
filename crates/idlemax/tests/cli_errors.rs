//! CLI recovery messages must identify the action that repairs the actual failure.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const STATE: &str = "state with spaces";

fn run(root: &Path, args: &[&str], token: Option<&str>) -> Output {
    let state = root.join(STATE);
    assert!(state.starts_with(std::env::temp_dir()));
    std::fs::create_dir_all(&state).unwrap();
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env_clear()
        .env("HOME", root)
        .env("IDLEMAX_HOME", state);
    if let Some(token) = token {
        command.env("IDLEMAX_TOKEN", token);
    }
    command.output().unwrap()
}

#[test]
fn token_errors_name_the_actual_problem_and_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(run(tmp.path(), &["init"], None).status.success());
    let token = tmp.path().join(STATE).join("token");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = run(tmp.path(), &["status", "--json"], None);
    assert_eq!(out.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["error"].as_str().unwrap().contains("mode 0600"));
    assert_eq!(value["help"], format!("chmod 600 '{}'", token.display()));

    let out = run(tmp.path(), &["status", "--json"], Some("short"));
    assert_eq!(out.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let message = value["error"].as_str().unwrap();
    assert!(message.contains("IDLEMAX_TOKEN"), "{value}");
    assert!(
        message.contains("too short") && message.contains("at least 32"),
        "{value}"
    );
    assert!(!message.contains("set but empty"), "{value}");
    assert!(value["help"]
        .as_str()
        .unwrap()
        .contains("unset IDLEMAX_TOKEN"));
}
