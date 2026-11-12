//! End to end: a coordinator in a temp IDLEMAX_HOME on a spare port gets one heartbeat, then
//! the CLI switches the device on and off; an agent in a second temp home that holds only its
//! device credential runs one plain job. Nothing touches the real home.
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
    /// The owner's home gets the owner token from the environment; a server's has only the
    /// token file it was given.
    owner: bool,
}

impl Home {
    fn new() -> Self {
        Self::with_owner(true)
    }

    fn server() -> Self {
        Self::with_owner(false)
    }

    fn with_owner(owner: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (home, state) = (dir.path().join("home"), dir.path().join("state"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        Self {
            _dir: dir,
            home,
            state,
            owner,
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
        if !self.owner {
            cmd.env_remove("IDLEMAX_TOKEN");
        }
        cmd
    }

    fn spawn(&self, name: &str) -> Proc {
        let log = self.home.join(format!("{name}.out"));
        let out = File::create(&log).unwrap();
        let child = self
            .cmd(&[name])
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        Proc { child, log }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(&[args, &["--json"]].concat());
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// A coordinator on a spare port that has seen one heartbeat from `device`, and its URL.
    async fn coord_with(&self, device: &str) -> (Proc, String) {
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
        let proc = self.spawn("coord");
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
        (proc, coord)
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
    let (_coord, _) = h.coord_with("my box").await;
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

/// Guard thresholds no healthy machine breaches, as in `tests/e2e.rs`.
const PERMISSIVE_GUARD: &str = r#"
max_jobs = 1
ram_budget_mib = 256
user_idle_min_secs = 0
require_ac_power = false
min_battery_pct = 0.0
pause_on_thermal = false
memory_pressure_pause = "critical"
memory_pressure_kill = "critical"
mem_free_pct_pause = 0.0
mem_free_pct_kill = 0.0
mem_available_mib_pause = 0
mem_available_mib_kill = 0
swap_growth_pause = { mib = 1048576, window_secs = 60 }
swap_growth_kill = { mib = 1048576, window_secs = 300 }
swap_used_mib_pause = 1048576
swap_used_mib_kill = 1048576
swap_in_pages_per_sec_pause = 1000000000.0
psi_mem_some_pause = 100.0
psi_mem_full_kill = 100.0
psi_io_full_pause = 100.0
psi_io_full_kill = 100.0
load_per_cpu_pause = 100000.0
load_per_cpu_kill = 100000.0
load_1m_pause = 100000.0
load_1m_kill = 100000.0
disk_free_mib_pause = 0
disk_free_mib_kill = 0
"#;

/// Polls `probe` every 200 ms until it gives a value, or fails after `secs`.
fn wait_for<T>(what: &str, secs: u64, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test]
async fn an_agent_with_its_own_credential_runs_a_plain_job_and_no_owner_command() {
    let owner = Home::new();
    let (_coord, coord) = owner.coord_with("mac").await;
    let raw = owner.run(&["device", "credential", "box", "--raw"]);
    assert!(raw.status.success(), "{}", text(&raw));

    // The real setup flow: the credential arrives on stdin, never in an argument.
    let server = Home::server();
    let mut init = server
        .cmd(&["init", "--coord-url", &coord, "--token-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(init.stdin.as_mut().unwrap(), &raw.stdout).unwrap();
    let init = init.wait_with_output().unwrap();
    assert!(init.status.success(), "{}", text(&init));
    std::fs::write(
        server.state.join("config.toml"),
        format!(
            "[coordinator]\nurl = \"{coord}\"\n\
             [agent]\ndevice_id = \"box\"\nlisten = \"127.0.0.1:{}\"\n\
             sample_secs = 1\nheartbeat_secs = 1\nadvisor_inputs_secs = 0\n\
             dispatch_inputs_secs = 0\ncommand_allowlist = [\"/bin/echo\"]\n\
             [advisor]\nenabled = false\n[guard]\n{PERMISSIVE_GUARD}",
            spare_port()
        ),
    )
    .unwrap();
    let _agent = server.spawn("agent");
    wait_for("the server online", 30, || {
        let status = owner.json(&["status"]);
        let devices = status["devices"].as_array()?.clone();
        devices
            .iter()
            .any(|d| d["info"]["id"] == "box" && d["online"] == true)
            .then_some(())
    });

    let added = owner.run(&[
        "job",
        "add",
        "--json",
        "--device",
        "box",
        "--mem",
        "64",
        "--",
        "/bin/echo",
        "hi",
    ]);
    assert!(added.status.success(), "{}", text(&added));
    let job: Value = serde_json::from_slice(&added.stdout).unwrap();
    let id = job["id"].as_i64().unwrap_or_else(|| panic!("{job}"));
    wait_for("the job to succeed", 30, || {
        let jobs = owner.json(&["jobs"]);
        let jobs = jobs.as_array().or(jobs["jobs"].as_array())?.clone();
        let job = jobs.iter().find(|j| j["id"] == id)?;
        (job["status"] == "succeeded").then_some(())
    });

    let status = server.run(&["status"]);
    assert!(!status.status.success());
    assert!(
        text(&status).contains("opens only the agent routes"),
        "{}",
        text(&status)
    );
    let doctor = server.run(&["doctor", "--json"]);
    let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let checks = report["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("{report}"));
    let coordinator = checks.iter().find(|c| c["id"] == "coordinator").unwrap();
    assert_eq!(coordinator["level"], "ok", "{coordinator}");
    assert!(coordinator["detail"]
        .as_str()
        .unwrap()
        .contains("its own credential"));
}
