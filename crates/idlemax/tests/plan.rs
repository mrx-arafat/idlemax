//! End to end: a coordinator in a temp IDLEMAX_HOME on a spare port gets one heartbeat with a
//! worker room and one dispatch feed; then the CLI switches the project on and shows the
//! plan. No agent runs, and nothing is launched.
#![cfg(unix)]

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const TOKEN: &str = "plan-test-token-0123456789abcdef0123456789";

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

async fn post(http: &reqwest::Client, url: String, body: Value) {
    let res = http
        .post(url)
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.text().await.unwrap());
}

#[tokio::test]
async fn plan_lists_a_roadmap_item_as_not_judged_and_launches_nothing() {
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
             [agent]\nadvisor_inputs_secs = 0\ndispatch_inputs_secs = 0\n\
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
    let heartbeat = json!({
        "device": {"id": "mac", "hostname": "mac", "os": "macos"},
        "room": {"state": "run", "reason": "run: memory 70% used", "open_slots": 2, "at_ms": 0}
    });
    post(&http, format!("{coord}/v1/agent/heartbeat"), heartbeat).await;
    post(&http, format!("{coord}/v1/devices/mac/on"), json!(null)).await;
    let feed = json!({"device": "mac", "herdr": true, "repos": [{
        "path": "/code/app", "name": "app", "default_branch": "main",
        "roadmap": [{"title": "Faster sync", "done_when": "sync takes under 1 s"}]
    }]});
    post(
        &http,
        format!("{coord}/v1/agent/dispatch-inputs"),
        feed.clone(),
    )
    .await;

    cli(&home, &state, &["project", "on", "app"]);
    // The next feed carries the roadmap of the switched-on project.
    post(&http, format!("{coord}/v1/agent/dispatch-inputs"), feed).await;
    let plan = cli(&home, &state, &["plan"]);
    assert_eq!(plan["mode"], "propose");
    assert_eq!(plan["devices"][0]["picks"], json!([]));
    assert_eq!(plan["devices"][0]["empty"]["count"], 2);
    assert_eq!(plan["not_judged"][0]["title"], "Faster sync", "{plan}");
    assert_eq!(cli(&home, &state, &["jobs"]), json!([]));
}
