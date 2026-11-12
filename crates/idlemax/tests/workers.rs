//! End to end: the owner's `idlemax stop`, `idlemax start` and `idlemax review` against a
//! coordinator in a temp IDLEMAX_HOME on a spare port. No agent runs, and no worker exists.
#![cfg(unix)]

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const TOKEN: &str = "workers-test-token-0123456789abcdef0123456";

struct Proc {
    child: Child,
    log: PathBuf,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            eprintln!("--- coord log ---\n{log}");
        }
    }
}

/// Refuses to run the binary against anything but a temp state dir.
fn isolated(cmd: &mut Command, home: &Path, state: &Path) {
    let real = std::env::var_os("HOME").map(|h| Path::new(&h).join(".idlemax"));
    assert!(state.starts_with(std::env::temp_dir()));
    assert_ne!(Some(state.to_path_buf()), real);
    cmd.env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env("IDLEMAX_TOKEN", TOKEN)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("IDLEMAX_COORD")
        .stdin(Stdio::null());
}

fn cli(home: &Path, state: &Path, args: &[&str]) -> Value {
    let mut cmd = Command::new(BIN);
    isolated(&mut cmd, home, state);
    let out = cmd.args(args).arg("--json").output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[tokio::test]
async fn stop_and_start_with_no_worker_succeed_and_report_none() {
    let dir = tempfile::tempdir().unwrap();
    let (home, state) = (dir.path().join("home"), dir.path().join("state"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let coord = format!("http://127.0.0.1:{port}");
    std::fs::write(
        state.join("config.toml"),
        format!(
            "[coordinator]\nlisten = \"127.0.0.1:{port}\"\nurl = \"{coord}\"\n\
             [advisor]\nenabled = false\n"
        ),
    )
    .unwrap();
    let log = home.join("coord.out");
    let mut cmd = Command::new(BIN);
    isolated(&mut cmd, &home, &state);
    let out = File::create(&log).unwrap();
    let child = cmd
        .arg("coord")
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .spawn()
        .unwrap();
    let _coord = Proc { child, log };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while http.get(format!("{coord}/healthz")).send().await.is_err() {
        assert!(Instant::now() < deadline, "coordinator did not start");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let stopped = cli(&home, &state, &["stop", "--reason", "test"]);
    assert_eq!(stopped["workers"].as_array().unwrap().len(), 0);
    assert_eq!(stopped["stop_in_force"], true);
    let started = cli(&home, &state, &["start"]);
    assert_eq!(started["workers"].as_array().unwrap().len(), 0);
    assert_eq!(started["paused"].as_array().unwrap().len(), 0);

    let review = cli(&home, &state, &["review"]);
    assert_eq!(review, serde_json::json!([]));
    let mut cmd = Command::new(BIN);
    isolated(&mut cmd, &home, &state);
    let out = cmd.args(["review", "discard", "5"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("--yes"), "{text}");
}
