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
         sample_secs = 1\nheartbeat_secs = 1\nadvisor_inputs_secs = 0\ncommand_allowlist = [\"/bin/echo\"]\n\n\
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

    let _agent = spawn("agent", &home, &state);
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
