//! End to end: `idlemax coord` and `idlemax agent` in a temp IDLEMAX_HOME run one tiny job.
#![cfg(unix)]

use std::fs::File;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use idlemax_core::protocol::{ClusterStatus, LocalStatus};
use idlemax_core::types::{Job, JobStatus};
use idlemax_core::Paths;
use reqwest::{header, Client, StatusCode};
use serde::de::DeserializeOwned;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const DEVICE: &str = "e2e-device";
const MARKER: &str = "idlemax-e2e-ok";

/// Guard thresholds no healthy machine breaches, so the test runs on a busy laptop or CI runner.
/// Critical macOS memory pressure still pauses: the test must not add load to a machine in trouble.
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
load_1m_pause = 100000.0
load_1m_kill = 100000.0
disk_free_mib_pause = 0
disk_free_mib_kill = 0
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
            let tail: Vec<_> = log.lines().rev().take(60).collect();
            eprintln!("--- {} log (last 60 lines, newest first) ---", self.name);
            for line in tail {
                eprintln!("{line}");
            }
        }
    }
}

/// Refuses to start a binary against anything but the temp state dir (docs/PLAN.md rule 5).
fn assert_isolated(state: &Path) {
    let real = std::env::var_os("HOME").map(|h| Path::new(&h).join(".idlemax"));
    assert!(
        state.starts_with(std::env::temp_dir()),
        "state dir must be under the temp dir"
    );
    assert_ne!(
        Some(state.to_path_buf()),
        real,
        "refusing to run against the real IDLEMAX_HOME"
    );
}

fn spawn(name: &'static str, home: &Path, state: &Path) -> Proc {
    assert_isolated(state);
    let log = home.join(format!("{name}.out"));
    let out = File::create(&log).unwrap();
    let child = Command::new(BIN)
        .arg(name)
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env_remove("IDLEMAX_TOKEN")
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

/// Authenticated JSON client for one base URL.
struct Api {
    http: Client,
    base: String,
    auth: String,
}

impl Api {
    /// None on any transport error, non-2xx status or bad JSON, so callers can poll.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Option<T> {
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .header(header::AUTHORIZATION, &self.auth)
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            return None;
        }
        res.json().await.ok()
    }
}

/// Runs `idlemax <args>` against the temp state dir, feeding `stdin` when given. The checks
/// below pin the human tables, which a pipe would otherwise replace with TOON.
fn idlemax(home: &Path, state: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    use std::io::Write;
    assert_isolated(state);
    let mut child = Command::new(BIN)
        .args(["--format", "table"])
        .args(args)
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env_remove("IDLEMAX_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        pipe.write_all(text.as_bytes()).unwrap();
    }
    drop(pipe);
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `idlemax job add` as an agent uses it: flags, --wait, --key, --timeout and stdin.
fn job_add_cli(home: &Path, state: &Path) {
    let run = |args: &[&str], stdin: Option<&str>| {
        let out = idlemax(home, state, args, stdin);
        (out.status.code(), text(&out.stdout), text(&out.stderr))
    };

    let (code, out, err) = run(
        &[
            "job",
            "add",
            "--wait",
            "--timeout",
            "60",
            "--mem",
            "64",
            "--",
            "/bin/echo",
            "hi-from-wait",
        ],
        None,
    );
    assert_eq!(code, Some(0), "echo: {out} {err}");
    assert!(
        out.contains("succeeded: echo\nexit code: 0\nran: "),
        "{out}"
    );
    assert!(out.contains("hi-from-wait"), "log end is printed: {out}");

    let (code, out, err) = run(
        &[
            "job",
            "add",
            "--wait",
            "--timeout",
            "30",
            "--mem",
            "64",
            "--",
            "/usr/bin/false",
        ],
        None,
    );
    // A non-zero exit is the job's own result: `failed` after one run, never re-queued
    // (a retry would back off 2 minutes and outlast the timeout).
    assert_eq!(code, Some(1), "a failing command exits 1: {out} {err}");
    assert!(out.contains("failed: false\nexit code: 1"), "{out}");
    assert!(!err.contains("queued again"), "{err}");

    // Paused, nothing runs: a key queues once, and a short wait times out.
    assert!(run(&["pause", "--reason", "e2e"], None).0 == Some(0));
    let queue = [
        "job",
        "add",
        "--key",
        "e2e-key",
        "--mem",
        "64",
        "--",
        "/bin/echo",
        "keyed",
    ];
    let (_, first, _) = run(&queue, None);
    let (code, second, _) = run(&queue, None);
    assert_eq!(code, Some(0));
    let first_line = first.lines().next().unwrap();
    assert!(first_line.contains("queued: echo"), "{first}");
    assert_eq!(
        second.lines().next().unwrap(),
        format!("{first_line} - already queued (no-op)")
    );
    let (code, out, _) = run(
        &[
            "job",
            "add",
            "--wait",
            "--timeout",
            "1",
            "--mem",
            "64",
            "--",
            "/bin/echo",
            "late",
        ],
        None,
    );
    assert_eq!(code, Some(124), "timed out waiting: {out}");
    assert!(out.contains("still queued after waiting"), "{out}");
    assert!(run(&["resume"], None).0 == Some(0));

    // Waiting on a retried key waits on the job that already holds it.
    let (code, out, err) = run(
        &[
            "job",
            "add",
            "--wait",
            "--key",
            "e2e-key",
            "--mem",
            "64",
            "--",
            "/bin/echo",
            "keyed",
        ],
        None,
    );
    assert_eq!(code, Some(0), "{out} {err}");
    assert!(err.contains("already queued (no-op)"), "{err}");
    assert!(
        out.contains("succeeded: echo") && out.contains("keyed"),
        "{out}"
    );

    let spec = "name = \"from-stdin\"\ncommand = [\"/bin/echo\", \"stdin-ok\"]\nmem_mb = 64\n";
    let (code, out, err) = run(
        &["job", "add", "-", "--wait", "--timeout", "60"],
        Some(spec),
    );
    assert_eq!(code, Some(0), "stdin spec: {out} {err}");
    assert!(
        out.contains("succeeded: from-stdin") && out.contains("stdin-ok"),
        "{out}"
    );
}

/// A job whose agent dies mid-run is not at fault: the restarted agent reaps the orphan and
/// the coordinator re-queues the job with a backoff instead of failing it.
async fn agent_death_is_retried(home: &Path, state: &Path, agent: &mut Proc, api: &Api) {
    let out = idlemax(
        home,
        state,
        &[
            "job",
            "add",
            "--name",
            "dies",
            "--mem",
            "64",
            "--",
            "/bin/sleep",
            "60",
        ],
        None,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let job_named = |status: Option<JobStatus>| async move {
        let jobs: Vec<Job> = api.get("/v1/jobs?limit=50").await?;
        jobs.into_iter()
            .find(|j| j.spec.name == "dies" && status.is_none_or(|s| j.status == s))
    };
    let running = wait_for("the sleeping job to run", Duration::from_secs(30), || {
        job_named(Some(JobStatus::Running))
    })
    .await;

    agent.child.kill().unwrap();
    agent.child.wait().unwrap();
    *agent = spawn("agent", home, state);

    let queued = wait_for("the job to be re-queued", Duration::from_secs(60), || {
        job_named(Some(JobStatus::Queued))
    })
    .await;
    assert_eq!((queued.id, queued.attempts), (running.id, 1));
    let why = queued.last_error.as_deref().unwrap_or_default();
    assert!(
        why.contains("orphaned") && why.contains("attempt 2 of 3 next"),
        "{why}"
    );
    assert!(queued.not_before_ms.is_some(), "retried with a backoff");
    let id = running.id.to_string();
    assert!(idlemax(home, state, &["job", "cancel", &id], None)
        .status
        .success());
}

/// Polls `probe` every 200 ms until it returns Some, or panics after `timeout`.
async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = probe().await {
            return v;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn coord_and_agent_run_a_tiny_job() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    let paths = Paths::new(&state);
    std::fs::create_dir_all(&state).unwrap();

    let (coord_port, agent_port) = (free_port(), free_port());
    let coord = format!("http://127.0.0.1:{coord_port}");
    // Advisor and its feed off: they would call the real `gh` and read session dirs.
    let config = format!(
        "[coordinator]\nlisten = \"127.0.0.1:{coord_port}\"\nurl = \"{coord}\"\n\n\
         [agent]\ndevice_id = \"{DEVICE}\"\nlisten = \"127.0.0.1:{agent_port}\"\n\
         sample_secs = 1\nheartbeat_secs = 1\nadvisor_inputs_secs = 0\ncommand_allowlist = [\"/bin/echo\", \"/usr/bin/false\", \"/bin/sleep\"]\n\n\
         [advisor]\nenabled = false\n\n\
         [guard]\n{PERMISSIVE_GUARD}"
    );
    std::fs::write(paths.config_file(), config).unwrap();

    // The real first-run flow: init keeps our config and creates the dirs and the token.
    assert_isolated(&state);
    let init = Command::new(BIN)
        .arg("init")
        .env("HOME", &home)
        .env("IDLEMAX_HOME", &state)
        .env_remove("IDLEMAX_TOKEN")
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "idlemax init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let token = std::fs::read_to_string(paths.token_file())
        .unwrap()
        .trim()
        .to_string();
    assert!(!token.is_empty());

    let http = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let auth = format!("Bearer {token}");
    let api = &Api {
        http: http.clone(),
        base: coord.clone(),
        auth: auth.clone(),
    };

    let _coord = spawn("coord", &home, &state);
    wait_for("coordinator /healthz", Duration::from_secs(30), || {
        let http = &http;
        let url = format!("{coord}/healthz");
        async move {
            http.get(url)
                .send()
                .await
                .ok()?
                .status()
                .is_success()
                .then_some(())
        }
    })
    .await;

    // Dashboard is public and locked down; the API is not.
    let page = http.get(format!("{coord}/")).send().await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(page.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    assert!(page
        .text()
        .await
        .unwrap()
        .contains("<title>idlemax</title>"));
    let anon = http.get(format!("{coord}/v1/status")).send().await.unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    let mut agent = spawn("agent", &home, &state);
    wait_for("device online", Duration::from_secs(30), || async move {
        let status: ClusterStatus = api.get("/v1/status").await?;
        status
            .devices
            .iter()
            .any(|d| d.info.id == DEVICE && d.online)
            .then_some(())
    })
    .await;

    let spec = serde_json::json!({
        "name": "e2e-echo",
        "command": ["/bin/echo", MARKER],
        "cost": "cpu_light",
        "mem_mb": 64,
        "max_runtime_secs": 60,
        "max_attempts": 1,
    });
    let res = http
        .post(format!("{coord}/v1/jobs"))
        .header(header::AUTHORIZATION, &auth)
        .json(&spec)
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "POST /v1/jobs: {}", res.status());
    let queued: Job = res.json().await.unwrap();
    let id = queued.id;

    let job = wait_for("job to finish", Duration::from_secs(90), || async move {
        let jobs: Vec<Job> = api.get("/v1/jobs?limit=50").await?;
        jobs.into_iter()
            .find(|j| j.id == id && j.status.is_terminal())
    })
    .await;
    assert_eq!(
        job.status,
        JobStatus::Succeeded,
        "job ended as {:?}: {:?}",
        job.status,
        job.last_error
    );
    assert_eq!(job.exit_code, Some(0));
    assert_eq!(job.device.as_deref(), Some(DEVICE));

    let log = std::fs::read_to_string(paths.job_log_file(job.id)).unwrap();
    assert!(log.contains(MARKER), "job log lacks the marker: {log}");
    let inbox = Path::new(
        job.output_path
            .as_deref()
            .expect("output_path set on finish"),
    );
    assert!(
        inbox.starts_with(paths.inbox_dir()) && inbox.is_file(),
        "inbox note {}",
        inbox.display()
    );

    // Verify CLI logs command
    let logs_out = Command::new(BIN)
        .args(["logs", &job.id.to_string()])
        .env("HOME", &home)
        .env("IDLEMAX_HOME", &state)
        .output()
        .unwrap();
    assert!(
        logs_out.status.success(),
        "logs command failed: {}",
        String::from_utf8_lossy(&logs_out.stderr)
    );
    let logs_str = String::from_utf8_lossy(&logs_out.stdout);
    assert!(logs_str.contains(MARKER));
    assert!(logs_str.contains("=== idlemax: succeeded (exit 0) ==="));

    // Verify CLI inbox list command
    let inbox_out = Command::new(BIN)
        .args(["inbox"])
        .env("HOME", &home)
        .env("IDLEMAX_HOME", &state)
        .output()
        .unwrap();
    assert!(
        inbox_out.status.success(),
        "inbox command failed: {}",
        String::from_utf8_lossy(&inbox_out.stderr)
    );
    let inbox_str = String::from_utf8_lossy(&inbox_out.stdout);
    assert!(inbox_str.contains(&job.id.to_string()));
    assert!(inbox_str.contains("e2e-echo"));
    assert!(inbox_str.contains("succeeded"));

    // Verify CLI inbox show command
    let show_out = Command::new(BIN)
        .args(["inbox", "show", &job.id.to_string()])
        .env("HOME", &home)
        .env("IDLEMAX_HOME", &state)
        .output()
        .unwrap();
    assert!(
        show_out.status.success(),
        "inbox show command failed: {}",
        String::from_utf8_lossy(&show_out.stderr)
    );
    let show_str = String::from_utf8_lossy(&show_out.stdout);
    assert!(show_str.contains("# e2e-echo: succeeded"));
    assert!(show_str.contains(MARKER));

    job_add_cli(&home, &state);
    agent_death_is_retried(&home, &state, &mut agent, api).await;

    let agent_api = Api {
        http,
        base: format!("http://127.0.0.1:{agent_port}"),
        auth,
    };
    let local: LocalStatus = agent_api
        .get("/v1/local/status")
        .await
        .expect("agent local status");
    assert_eq!(local.device, DEVICE);
    assert!(local.coordinator_reachable);
}
