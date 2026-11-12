//! End to end: a coordinator in a temp IDLEMAX_HOME on a spare port gets one heartbeat, then
//! the CLI switches the device on and off. No agent runs, and nothing is launched.
#![cfg(unix)]

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const TOKEN: &str = "devices-test-token-0123456789abcdef01234567";
const LAUNCHED: &str = "this call comes from a job or worker that idlemax started";

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

struct Home {
    _dir: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (home, state) = (dir.path().join("home"), dir.path().join("state"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        Self {
            _dir: dir,
            home,
            state,
        }
    }

    /// Refuses to run the binary against anything but a temp state dir.
    fn cmd(&self, args: &[&str]) -> Command {
        let real = std::env::var_os("HOME").map(|h| Path::new(&h).join(".idlemax"));
        assert!(self.state.starts_with(std::env::temp_dir()));
        assert_ne!(Some(self.state.clone()), real);
        let mut cmd = Command::new(BIN);
        cmd.env("HOME", &self.home)
            .env("IDLEMAX_HOME", &self.state)
            .env("IDLEMAX_TOKEN", TOKEN)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("IDLEMAX_COORD")
            .env_remove("IDLEMAX_JOB_ID")
            .args(args)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(&[args, &["--json"]].concat());
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// A coordinator on a spare port that has seen one heartbeat from `device`.
    async fn coord_with(&self, device: &str) -> Proc {
        let port = spare_port();
        let coord = format!("http://127.0.0.1:{port}");
        std::fs::write(
            self.state.join("config.toml"),
            format!(
                "[coordinator]\nlisten = \"127.0.0.1:{port}\"\nurl = \"{coord}\"\n\
                 [agent]\nadvisor_inputs_secs = 0\ndispatch_inputs_secs = 0\n\
                 [advisor]\nenabled = false\n"
            ),
        )
        .unwrap();
        let log = self.home.join("coord.out");
        let out = File::create(&log).unwrap();
        let child = self
            .cmd(&["coord"])
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        let proc = Proc { child, log };
        let http = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while http.get(format!("{coord}/healthz")).send().await.is_err() {
            assert!(Instant::now() < deadline, "coordinator did not start");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let res = http
            .post(format!("{coord}/v1/agent/heartbeat"))
            .bearer_auth(TOKEN)
            .json(&json!({"device": {"id": device, "hostname": "box", "os": "linux"}}))
            .send()
            .await
            .unwrap();
        assert!(res.status().is_success());
        proc
    }
}

fn spare_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[tokio::test]
async fn device_on_switches_once_and_status_shows_it() {
    let h = Home::new();
    let _coord = h.coord_with("my box").await;
    let first = h.json(&["device", "on", "my box"]);
    assert_eq!(
        (&first["enabled"], &first["changed"]),
        (&json!(true), &json!(true))
    );
    let again = h.json(&["device", "on", "my box"]);
    assert_eq!(again["changed"], false);
    let out = h.run(&["device", "on", "my box"]);
    assert!(text(&out).contains("already on (no-op)"), "{}", text(&out));
    let status = h.json(&["status"]);
    assert_eq!(status["devices"][0]["enabled"], true, "{status}");

    let unknown = h.run(&["device", "off", "nope"]);
    assert!(!unknown.status.success());
    let said = text(&unknown);
    assert!(said.contains("known devices are my box"), "{said}");
    assert!(said.contains("idlemax status"), "{said}");
}

#[test]
fn a_launched_process_may_switch_a_device_off_but_not_on() {
    let h = Home::new();
    let closed = format!("http://127.0.0.1:{}", spare_port());
    let marked = |word: &str| {
        h.cmd(&["--coord", &closed, "device", word, "box"])
            .env("IDLEMAX_JOB_ID", "7")
            .output()
            .unwrap()
    };
    let on = marked("on");
    assert!(!on.status.success());
    assert!(text(&on).contains(LAUNCHED), "{}", text(&on));
    let off = marked("off");
    assert!(!off.status.success());
    assert!(!text(&off).contains(LAUNCHED), "{}", text(&off));
    assert!(text(&off).contains("unreachable"), "{}", text(&off));
}
