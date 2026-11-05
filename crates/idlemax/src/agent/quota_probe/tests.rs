use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use idlemax_core::quota::rolling_5h;
use idlemax_core::types::{ProbeSource, QuotaWindowKind};

use super::*;

fn capture_script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/claude-statusline-capture.sh")
}

#[test]
fn read_claude_statusline_parses_nested_rate_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("claude-statusline.json");
    let json = serde_json::json!({
        "cwd": "/workspace/project",
        "model": {
            "id": "claude-3-7-sonnet",
            "display_name": "Claude 3.7 Sonnet"
        },
        "rate_limits": {
            "five_hour": {
                "used_percentage": 35.5,
                "resets_at": 1741219200.0
            },
            "seven_day": {
                "used_percentage": 20.0,
                "resets_at": 1741737600.0
            }
        }
    });
    fs::write(&file, json.to_string()).unwrap();

    let snaps = read_claude_statusline(&file, "claude-max").unwrap();
    assert_eq!(snaps.len(), 2);

    let five = snaps.iter().find(|s| s.window == rolling_5h()).unwrap();
    assert_eq!(five.account, "claude-max");
    assert!((five.used_pct - 35.5).abs() < f64::EPSILON);
    assert_eq!(five.resets_at_ms, Some(1741219200000));
    assert_eq!(five.source, ProbeSource::ClaudeStatusline);

    let week = snaps
        .iter()
        .find(|s| s.window == QuotaWindowKind::Weekly)
        .unwrap();
    assert_eq!(week.account, "claude-max");
    assert!((week.used_pct - 20.0).abs() < f64::EPSILON);
    assert_eq!(week.resets_at_ms, Some(1741737600000));
    assert_eq!(week.source, ProbeSource::ClaudeStatusline);
}

#[test]
fn read_claude_statusline_parses_top_level_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("claude-statusline.json");
    let json = serde_json::json!({
        "five_hour": {
            "used_percentage": 10.0,
            "resets_at": 1741219200.0
        }
    });
    fs::write(&file, json.to_string()).unwrap();

    let snaps = read_claude_statusline(&file, "claude-acct").unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].account, "claude-acct");
    assert_eq!(snaps[0].window, rolling_5h());
    assert!((snaps[0].used_pct - 10.0).abs() < f64::EPSILON);
    assert_eq!(snaps[0].resets_at_ms, Some(1741219200000));
}

#[test]
fn read_claude_statusline_missing_file_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nonexistent.json");
    let err = read_claude_statusline(&missing, "claude-max").unwrap_err();
    assert!(err.is_not_found());
}

#[test]
fn claude_statusline_capture_wrapper_proves_transparent_and_fast() {
    let script = capture_script_path();
    assert!(script.is_file(), "wrapper script exists");

    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    let agent_dir = state_dir.join("agent");
    fs::create_dir_all(&agent_dir).unwrap();

    let fixture = serde_json::json!({
        "cwd": "/tmp/project",
        "rate_limits": {
            "five_hour": {
                "used_percentage": 42.0,
                "resets_at": 1900000000.0
            },
            "seven_day": {
                "used_percentage": 15.0,
                "resets_at": 1900500000.0
            }
        }
    })
    .to_string();

    let capture_file = agent_dir.join("claude-statusline.json");

    // 1. Transparent passthrough: visible stdout is byte-identical to wrapped command
    let mut child = Command::new(&script)
        .arg("printf")
        .arg("CUSTOM_STATUSLINE_OUTPUT")
        .env("IDLEMAX_HOME", &state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(fixture.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "CUSTOM_STATUSLINE_OUTPUT"
    );

    // Snapshot landed in temp IDLEMAX_HOME and parses correctly
    assert!(capture_file.is_file());
    let snaps = read_claude_statusline(&capture_file, "claude-test").unwrap();
    assert_eq!(snaps.len(), 2);
    let five = snaps.iter().find(|s| s.window == rolling_5h()).unwrap();
    assert!((five.used_pct - 42.0).abs() < f64::EPSILON);
    fs::remove_file(&capture_file).unwrap();

    // 2. Failing wrapped command does not break or slow statusline capture
    let t0 = Instant::now();
    let mut child = Command::new(&script)
        .args(["sh", "-c", "printf 'err-out'; exit 1"])
        .env("IDLEMAX_HOME", &state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(fixture.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed.as_millis() < 1000,
        "failing command took too long: {elapsed:?}"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "err-out");
    assert!(
        capture_file.is_file(),
        "snapshot written on failing command"
    );
    fs::remove_file(&capture_file).unwrap();

    // 3. Missing wrapped command does not break or slow statusline capture
    let t0 = Instant::now();
    let mut child = Command::new(&script)
        .arg("/nonexistent/wrapped/command/path")
        .env("IDLEMAX_HOME", &state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(fixture.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed.as_millis() < 1000,
        "missing command took too long: {elapsed:?}"
    );
    assert!(output.stdout.is_empty());
    assert!(
        capture_file.is_file(),
        "snapshot written on missing command"
    );
    fs::remove_file(&capture_file).unwrap();

    // 4. No wrapped command args writes snapshot quickly and cleanly
    let t0 = Instant::now();
    let mut child = Command::new(&script)
        .env("IDLEMAX_HOME", &state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(fixture.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed.as_millis() < 1000,
        "no-arg command took too long: {elapsed:?}"
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        capture_file.is_file(),
        "snapshot written with no wrapped args"
    );
}

#[test]
fn interactive_use_detects_agy_session_newest_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let agy_root = tmp.path().join("gemini/antigravity-cli");
    let brain_dir = agy_root.join("brain");

    // Session 1: older session
    let s1_logs = brain_dir
        .join("11111111-1111-1111-1111-111111111111")
        .join(".system_generated/logs");
    fs::create_dir_all(&s1_logs).unwrap();
    let s1_file = s1_logs.join("transcript.jsonl");
    fs::write(&s1_file, "{\"step\": 1}\n").unwrap();

    // Session 2: newer session
    let s2_logs = brain_dir
        .join("22222222-2222-2222-2222-222222222222")
        .join(".system_generated/logs");
    fs::create_dir_all(&s2_logs).unwrap();
    let s2_file = s2_logs.join("transcript.jsonl");
    fs::write(&s2_file, "{\"step\": 2}\n").unwrap();

    let s2_mtime = mtime_ms(&s2_file).unwrap();

    let dirs = ProbeDirs {
        state_dir: tmp.path().join("state"),
        claude_dir: None,
        codex_dir: None,
        agy_dir: Some(agy_root),
    };

    let now_ms = s2_mtime + 5000;
    let active = interactive_use(&dirs, now_ms);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].0, "agy");
    assert_eq!(active[0].1, s2_mtime);

    // If now_ms is before file mtime, clamps to now_ms
    let clamped = interactive_use(&dirs, s2_mtime - 1000);
    assert_eq!(clamped[0].1, s2_mtime - 1000);
}
