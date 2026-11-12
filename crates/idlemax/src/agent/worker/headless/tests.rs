//! Tests: the headless host over a stub `claude` script in a temp dir; no real agent CLI runs.

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use idlemax_core::job_run::LogProgress;
use idlemax_core::worker::launch::env_problems;

use super::*;

/// The stub's first argument picks what it does; it reads its prompt to the end first.
const STUB: &str = r#"#!/bin/sh
case "$1" in
  echo) cat ;;
  limit) cat >/dev/null; echo "usage limit reached"; exit 1 ;;
  env) cat >/dev/null; env ;;
  sleep) cat >/dev/null; sleep 30 ;;
esac
"#;

struct Rig {
    _temp: tempfile::TempDir,
    paths: Paths,
    host: HeadlessHost,
}

fn rig() -> Rig {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("claude");
    std::fs::write(&stub, STUB).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let paths = Paths::new(temp.path().join("state"));
    let host = HeadlessHost::with_bin_dir(paths.clone(), bin);
    Rig {
        _temp: temp,
        paths,
        host,
    }
}

fn launch(rig: &Rig, mode: &str) -> Launch {
    Launch {
        name: "im-app-fix-1".into(),
        tab: "im: App".into(),
        folder: rig._temp.path().to_path_buf(),
        env: vec![
            ("IDLEMAX_JOB_ID".into(), "7".into()),
            ("IDLEMAX_JOB_TAG".into(), "42".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("IDLEMAX_TOKEN".into(), String::new()),
        ],
        cli: WorkerCli::Claude,
        args: vec![mode.into(), "--session-id".into(), "u-1".into()],
        job: 7,
        attempt: 2,
    }
}

fn until_done(rig: &Rig, handle: &Handle) -> HostState {
    let started = Instant::now();
    loop {
        let state = rig.host.state(handle).unwrap();
        if matches!(state.status, AgentStatus::Done | AgentStatus::Gone) {
            return state;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "{state:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn log(rig: &Rig) -> String {
    std::fs::read_to_string(rig.paths.job_log_file(7)).unwrap()
}

#[test]
fn a_prompt_reaches_the_child_and_its_output_ends_the_job_log_with_the_footer() {
    let rig = rig();
    let handle = rig.host.start(&launch(&rig, "echo")).unwrap();
    let before = rig.host.state(&handle).unwrap();
    assert_eq!(before.status, AgentStatus::Idle);
    assert_eq!(before.session.as_deref(), Some("u-1"));
    assert!(before.shell_pid.is_some() && before.exit_code.is_none());
    rig.host.send(&handle, "read the brief").unwrap();
    let after = until_done(&rig, &handle);
    assert_eq!(
        (after.status, after.exit_code),
        (AgentStatus::Done, Some(0))
    );
    let text = log(&rig);
    assert!(
        text.starts_with("=== idlemax job 7 attempt 2 started "),
        "{text}"
    );
    assert!(text.contains("read the brief"), "{text}");
    assert!(
        text.ends_with("=== idlemax: succeeded (exit 0) ===\n"),
        "{text}"
    );
    // `idlemax logs --follow` returns on this footer.
    assert!(LogProgress::default().push(text.as_bytes()));
    let again = rig.host.send(&handle, "twice").unwrap_err();
    assert!(matches!(again, HostError::AlreadySent { .. }), "{again}");
}

#[test]
fn an_error_exit_gives_its_code_and_the_last_lines_as_end_text() {
    let rig = rig();
    let handle = rig.host.start(&launch(&rig, "limit")).unwrap();
    rig.host.send(&handle, "go").unwrap();
    let state = until_done(&rig, &handle);
    assert_eq!(
        (state.status, state.exit_code),
        (AgentStatus::Done, Some(1))
    );
    assert_eq!(
        rig.host.end_text(&handle).as_deref(),
        Some("usage limit reached")
    );
    assert!(log(&rig).ends_with("=== idlemax: failed (exit 1) ===\n"));
}

#[test]
fn the_child_gets_the_job_environment_and_the_pairs_but_never_the_token() {
    let rig = rig();
    let mut launch = launch(&rig, "env");
    launch.env.push(("GH_TOKEN".into(), "secret".into()));
    let handle = rig.host.start(&launch).unwrap();
    rig.host.send(&handle, "go").unwrap();
    until_done(&rig, &handle);
    let text = log(&rig);
    let vars: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let has = |k: &str| vars.iter().any(|(name, _)| name == k);
    assert!(has("IDLEMAX_JOB_ID") && has("IDLEMAX_JOB_TAG") && has("GIT_TERMINAL_PROMPT"));
    assert!(!has("IDLEMAX_TOKEN"), "{text}");
    // Not an inherited name: the test process has it, the child does not.
    assert!(!has("CARGO_PKG_NAME"), "{text}");
    // A credential that does get into the child is what the launch's check reads and names.
    assert_eq!(env_problems(&vars), ["GH_TOKEN"]);
}

#[test]
fn a_stop_ends_the_process_group_and_the_footer_says_killed() {
    let rig = rig();
    let handle = rig.host.start(&launch(&rig, "sleep")).unwrap();
    rig.host.send(&handle, "go").unwrap();
    assert_eq!(
        rig.host.state(&handle).unwrap().status,
        AgentStatus::Working
    );
    assert!(rig.host.matches(&handle).unwrap());
    rig.host.stop(&handle).unwrap();
    let state = until_done(&rig, &handle);
    assert_eq!(state.exit_code, Some(128 + 15));
    assert!(!rig.host.matches(&handle).unwrap());
    assert!(log(&rig).ends_with("=== idlemax: killed (exit 143) ===\n"));
    rig.host.stop(&handle).unwrap();
}

#[test]
fn a_child_that_exits_before_its_prompt_is_gone() {
    let rig = rig();
    let handle = rig.host.start(&launch(&rig, "none")).unwrap();
    let state = until_done(&rig, &handle);
    assert_eq!(
        (state.status, state.exit_code),
        (AgentStatus::Gone, Some(0))
    );
    assert_eq!(
        session_of(&["--resume".into(), "s-9".into()]).as_deref(),
        Some("s-9")
    );
}

#[test]
fn a_test_host_never_searches_the_real_path() {
    let rig = rig();
    assert!(HeadlessHost::new(rig.paths.clone())
        .search_dirs()
        .is_empty());
    assert_eq!(rig.host.search_dirs(), [rig._temp.path().join("bin")]);
}

#[test]
fn a_start_that_cannot_spawn_ends_the_log_with_the_failure_footer() {
    let rig = rig();
    let stub = rig._temp.path().join("bin").join("claude");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(rig.host.start(&launch(&rig, "echo")).is_err());
    let text = log(&rig);
    assert!(
        text.ends_with("=== idlemax: failed (exit -1) ===\n"),
        "{text}"
    );
    assert!(LogProgress::default().push(text.as_bytes()));
}

#[test]
fn a_stale_handle_never_reaches_a_later_worker_with_the_same_pid() {
    let rig = rig();
    let first = rig.host.start(&launch(&rig, "sleep")).unwrap();
    rig.host.stop(&first).unwrap();
    until_done(&rig, &first);
    let second = rig.host.start(&launch(&rig, "sleep")).unwrap();
    assert_ne!(first.label, second.label);
    // The first worker's handle, as if the system had given its pid to the second one.
    let stale = Handle {
        pane: second.pane.clone(),
        ..first.clone()
    };
    assert_eq!(rig.host.state(&stale).unwrap().status, AgentStatus::Gone);
    rig.host.stop(&stale).unwrap();
    assert_eq!(rig.host.state(&second).unwrap().status, AgentStatus::Idle);
    rig.host.stop(&second).unwrap();
    until_done(&rig, &second);
}
