//! End to end: a coordinator and an agent run in separate temp IDLEMAX_HOMEs on 127.0.0.1.
//! The agent reads a fixture `gh` and a fixture Claude session dir; the coordinator's own
//! HOME is empty, so its advisor rules can only fire from what the agent sent.
#![cfg(unix)]

use std::fs::File;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use idlemax_core::types::Recommendation;
use reqwest::{header, Client};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const TOKEN: &str = "advisor-inputs-test-token";
const PR_TITLE: &str = "Review owner/app#7: Add retries";
const SESSION_TITLE: &str = "Session 'Fix login' in myproj still has failing tests";

const TRANSCRIPT: &str = r#"{"type":"user","cwd":"/work/myproj","message":{"content":"fix the login"}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"test result: FAILED. 1 passed; 1 failed"}]}}
{"type":"ai-title","aiTitle":"Fix login"}
"#;

/// A child process that is killed on drop and dumps its log if the test failed.
struct Proc {
    name: &'static str,
    child: Child,
    log: PathBuf,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            eprintln!("--- {} log (last 40 lines, newest first) ---", self.name);
            for line in log.lines().rev().take(40) {
                eprintln!("{line}");
            }
        }
    }
}

/// Refuses to start a binary against anything but a temp state dir.
fn assert_isolated(state: &Path) {
    let real = std::env::var_os("HOME").map(|h| Path::new(&h).join(".idlemax"));
    assert!(state.starts_with(std::env::temp_dir()));
    assert_ne!(Some(state.to_path_buf()), real);
}

fn spawn(name: &'static str, home: &Path, state: &Path, path_env: &str) -> Proc {
    assert_isolated(state);
    let log = home.join(format!("{name}.out"));
    let out = File::create(&log).unwrap();
    let child = Command::new(BIN)
        .arg(name)
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env("IDLEMAX_TOKEN", TOKEN)
        // The quota probe honors these; they must never point at real CLI state.
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env("PATH", path_env)
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .spawn()
        .unwrap();
    Proc { name, child, log }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A `gh` that answers the review-requested search with one PR idle for 5 days.
fn write_fake_gh(bin: &Path) {
    let ts = (chrono::Utc::now() - chrono::Duration::days(5)).to_rfc3339();
    let pr = format!(
        r#"[{{"number":7,"title":"Add retries","url":"https://github.test/owner/app/pull/7","repository":{{"nameWithOwner":"owner/app"}},"author":{{"login":"alice"}},"isDraft":false,"createdAt":"{ts}","updatedAt":"{ts}"}}]"#
    );
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n  *--review-requested*) printf '%s' '{pr}' ;;\n  *) printf '[]' ;;\nesac\n"
    );
    std::fs::create_dir_all(bin).unwrap();
    let path = bin.join("gh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A Claude session with failing tests, last active 5 hours ago.
fn write_session(home: &Path) {
    let dir = home.join(".claude/projects/-work-myproj");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("abc123.jsonl");
    std::fs::write(&file, TRANSCRIPT).unwrap();
    let five_hours_ago = SystemTime::now() - Duration::from_secs(5 * 3600);
    File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(five_hours_ago)
        .unwrap();
}

async fn refresh(http: &Client, coord: &str) -> Option<Vec<String>> {
    let res = http
        .post(format!("{coord}/v1/advice/refresh"))
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .send()
        .await
        .ok()?;
    let recs: Vec<Recommendation> = res.error_for_status().ok()?.json().await.ok()?;
    Some(recs.into_iter().map(|r| r.title).collect())
}

#[tokio::test]
async fn advisor_rules_fire_from_inputs_the_agent_sent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (coord_home, coord_state) = (root.join("coord-home"), root.join("coord-state"));
    let (agent_home, agent_state) = (root.join("agent-home"), root.join("agent-state"));
    for dir in [&coord_home, &coord_state, &agent_home, &agent_state] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let (coord_port, agent_port) = (free_port(), free_port());
    let coord = format!("http://127.0.0.1:{coord_port}");
    std::fs::write(
        coord_state.join("config.toml"),
        format!("[coordinator]\nlisten = \"127.0.0.1:{coord_port}\"\nurl = \"{coord}\"\n"),
    )
    .unwrap();
    std::fs::write(
        agent_state.join("config.toml"),
        format!(
            "[coordinator]\nurl = \"{coord}\"\n\n[agent]\ndevice_id = \"fixture-mac\"\n\
             listen = \"127.0.0.1:{agent_port}\"\nsample_secs = 1\nheartbeat_secs = 1\n"
        ),
    )
    .unwrap();
    let bin = root.join("bin");
    write_fake_gh(&bin);
    write_session(&agent_home);

    let system_path = std::env::var("PATH").unwrap_or_default();
    let http = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let _coord = spawn("coord", &coord_home, &coord_state, &system_path);
    let start = Instant::now();
    let before = loop {
        if let Some(titles) = refresh(&http, &coord).await {
            break titles;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "coordinator never answered"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(!before.iter().any(|t| t == PR_TITLE || t == SESSION_TITLE));

    let agent_path = format!("{}:{system_path}", bin.display());
    let _agent = spawn("agent", &agent_home, &agent_state, &agent_path);
    let start = Instant::now();
    loop {
        let titles = refresh(&http, &coord).await.unwrap_or_default();
        if titles.iter().any(|t| t == PR_TITLE) && titles.iter().any(|t| t == SESSION_TITLE) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "advice never fired; last titles: {titles:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
