//! End to end: `idlemax doctor` in a temp home reports a healthy setup, then each
//! broken case with its fix. The coordinator and agent run from this same binary.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const DEVICE: &str = "doctor-device";

/// A child process that is killed on drop.
struct Proc {
    child: Child,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Refuses to run the binary against anything but a temp state dir (docs/PLAN.md rule 5).
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Guard lines that can never trip on a real disk, so the healthy cases stay healthy.
const NO_DISK_GUARD: &str = "disk_free_mib_pause = 0\ndisk_free_mib_kill = 0\n";

/// `guard` is the whole `[guard]` body.
fn write_config(state: &Path, coord_port: u16, agent_port: u16, guard: &str, allow: &str) {
    let text = format!(
        "[coordinator]\nlisten = \"127.0.0.1:{coord_port}\"\nurl = \"http://127.0.0.1:{coord_port}\"\n\n\
         [agent]\ndevice_id = \"{DEVICE}\"\nlisten = \"127.0.0.1:{agent_port}\"\n\
         sample_secs = 1\nheartbeat_secs = 1\nadvisor_inputs_secs = 0\ndispatch_inputs_secs = 0\ncommand_allowlist = {allow}\n\n\
         [advisor]\nenabled = false\n\n\
         [guard]\n{guard}"
    );
    std::fs::write(state.join("config.toml"), text).unwrap();
}

/// Writes the token the way `idlemax init` does, so later checks keep passing.
fn write_token(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn service_unit(home: &Path) -> PathBuf {
    match cfg!(target_os = "linux") {
        true => home.join(".config/systemd/user/idlemax-agent.service"),
        false => home.join("Library/LaunchAgents/com.idlemax.agent.plist"),
    }
}

/// Runs `idlemax doctor` with a temp home and returns (exit code, parsed JSON).
fn doctor(home: &Path, state: &Path, extra: &[&str]) -> (i32, Value) {
    assert_isolated(state);
    let out = Command::new(BIN)
        .args(["doctor", "--json"])
        .args(extra)
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env_remove("IDLEMAX_TOKEN")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json is not JSON: {e}\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), json)
}

fn doctor_with_path(home: &Path, state: &Path, path: &str) -> (i32, Value) {
    assert_isolated(state);
    let out = Command::new(BIN)
        .args(["doctor", "--json"])
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env("PATH", path)
        .env_remove("IDLEMAX_TOKEN")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "doctor --json is not JSON: {e}\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.code().unwrap_or(-1), json)
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check in {report}"))
}

fn level(report: &Value, id: &str) -> String {
    check(report, id)["level"].as_str().unwrap().to_string()
}

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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn spawn(arg: &str, home: &Path, state: &Path) -> Proc {
    assert_isolated(state);
    let log = std::fs::File::create(home.join(format!("{arg}.out"))).unwrap();
    let child = Command::new(BIN)
        .arg(arg)
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env_remove("IDLEMAX_TOKEN")
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    Proc { child }
}

#[tokio::test]
async fn doctor_reports_healthy_and_every_broken_case() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    assert_isolated(&state);
    // A private state dir, as `idlemax init` leaves it.
    set_mode(&state, 0o700);

    let (coord_port, agent_port) = (free_port(), free_port());
    write_config(
        &state,
        coord_port,
        agent_port,
        NO_DISK_GUARD,
        "[\"/bin/echo\"]",
    );
    let unit = service_unit(&home);
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(&unit, "[Unit]\nDescription=idlemax agent\n").unwrap();

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
    let token_file = state.join("token");
    let token_text = std::fs::read_to_string(&token_file).unwrap();

    let _coord = spawn("coord", &home, &state);
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let health = format!("http://127.0.0.1:{coord_port}/healthz");
    let local = format!("http://127.0.0.1:{agent_port}/healthz");
    wait_for("coordinator /healthz", Duration::from_secs(30), || {
        let http = &http;
        let health = health.clone();
        async move {
            let ok = http.get(health).send().await.ok()?.status().is_success();
            ok.then_some(())
        }
    })
    .await;
    let _agent = spawn("agent", &home, &state);
    wait_for("agent /healthz", Duration::from_secs(30), || {
        let http = &http;
        let local = local.clone();
        async move {
            let ok = http.get(local).send().await.ok()?.status().is_success();
            ok.then_some(())
        }
    })
    .await;

    // The agent's first heartbeat flips its coordinator link to reachable.
    wait_for("agent heartbeat", Duration::from_secs(30), || {
        let (home, state) = (home.clone(), state.clone());
        async move { (level(&doctor(&home, &state, &[]).1, "agent") == "ok").then_some(()) }
    })
    .await;

    // Healthy: every check ok, exit 0.
    let (code, report) = doctor(&home, &state, &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["counts"]["fail"], 0, "{report}");
    assert_eq!(report["healthy"], true, "{report}");
    for id in [
        "home",
        "config",
        "token",
        "coordinator",
        "agent",
        "pause",
        "allowlist",
        "disk",
        "ai",
        "gh",
        "service",
    ] {
        assert_eq!(level(&report, id), "ok", "{id} in {report}");
    }

    // Broken: the token is readable by group and others.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (code, report) = doctor(&home, &state, &[]);
        assert_eq!(code, 1, "{report}");
        let token = check(&report, "token");
        assert_eq!(token["level"], "fail");
        assert!(token["detail"].as_str().unwrap().contains("mode 0644"));
        assert!(token["fix"].as_str().unwrap().contains("chmod 600"));
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    // Broken: the token file is gone.
    {
        std::fs::remove_file(&token_file).unwrap();
        let (code, report) = doctor(&home, &state, &[]);
        assert_eq!(code, 1, "{report}");
        let token = check(&report, "token");
        assert_eq!(token["level"], "fail");
        assert!(token["fix"].as_str().unwrap().contains("idlemax init"));
        // No token means the auth checks say so instead of pretending.
        assert_eq!(level(&report, "coordinator"), "warn");
        assert_eq!(level(&report, "agent"), "warn");
        write_token(&token_file, &token_text);
    }

    // Broken: an allowlisted program that is not installed.
    {
        write_config(
            &state,
            coord_port,
            agent_port,
            NO_DISK_GUARD,
            "[\"/bin/echo\", \"idlemax-no-such-binary\"]",
        );
        let (code, report) = doctor(&home, &state, &[]);
        assert_eq!(code, 1, "{report}");
        let allow = check(&report, "allowlist");
        assert_eq!(allow["level"], "fail");
        assert!(allow["detail"]
            .as_str()
            .unwrap()
            .contains("idlemax-no-such-binary"));
        assert!(allow["fix"]
            .as_str()
            .unwrap()
            .contains("agent.command_allowlist"));
        write_config(
            &state,
            coord_port,
            agent_port,
            NO_DISK_GUARD,
            "[\"/bin/echo\"]",
        );
    }

    // Broken: free space under the kill line fails, under the pause line warns.
    {
        write_config(
            &state,
            coord_port,
            agent_port,
            "disk_free_mib_pause = 0\ndisk_free_mib_kill = 1100000000000000\n",
            "[\"/bin/echo\"]",
        );
        let (code, report) = doctor(&home, &state, &[]);
        assert_eq!(code, 1, "{report}");
        let disk = check(&report, "disk");
        assert_eq!(disk["level"], "fail");
        assert!(disk["detail"]
            .as_str()
            .unwrap()
            .contains("running jobs are killed"));

        write_config(
            &state,
            coord_port,
            agent_port,
            "disk_free_mib_pause = 1100000000000000\ndisk_free_mib_kill = 0\n",
            "[\"/bin/echo\"]",
        );
        let (code, report) = doctor(&home, &state, &[]);
        assert_eq!(code, 0, "{report}");
        let disk = check(&report, "disk");
        assert_eq!(disk["level"], "warn");
        assert!(disk["detail"]
            .as_str()
            .unwrap()
            .contains("jobs will never start"));
        write_config(
            &state,
            coord_port,
            agent_port,
            NO_DISK_GUARD,
            "[\"/bin/echo\"]",
        );
    }

    // Held: the kill switch file warns with the exact rm, and a warn alone exits 0.
    {
        let pause_file = state.join("PAUSE");
        std::fs::write(&pause_file, "").unwrap();
        let report = wait_for("pause check to see PAUSE", Duration::from_secs(30), || {
            let (home, state) = (home.clone(), state.clone());
            async move {
                let (code, report) = doctor(&home, &state, &[]);
                (level(&report, "pause") == "warn").then_some((code, report))
            }
        })
        .await;
        assert_eq!(report.0, 0, "{}", report.1);
        let pause = check(&report.1, "pause");
        assert_eq!(
            pause["fix"],
            format!("rm {}", pause_file.display()),
            "{pause}"
        );
        std::fs::remove_file(&pause_file).unwrap();
        wait_for("pause check to clear", Duration::from_secs(30), || {
            let (home, state) = (home.clone(), state.clone());
            async move { (level(&doctor(&home, &state, &[]).1, "pause") == "ok").then_some(()) }
        })
        .await;
    }

    // Broken: the coordinator is not there (port closed right after binding it).
    {
        let closed = free_port();
        let url = format!("http://127.0.0.1:{closed}");
        let (code, report) = doctor(&home, &state, &["--coord", &url]);
        assert_eq!(code, 1, "{report}");
        let coord = check(&report, "coordinator");
        assert_eq!(coord["level"], "fail");
        let detail = coord["detail"].as_str().unwrap();
        assert!(
            detail.starts_with(&format!("{url} is not answering")),
            "{detail}"
        );
        assert!(!detail.contains("error sending request"), "{detail}");
        assert!(coord["fix"].as_str().unwrap().contains("idlemax coord"));
    }

    // A state dir open to other users is a warning with its fix; job logs and notes live there.
    {
        set_mode(&state, 0o755);
        let (code, report) = doctor(&home, &state, &[]);
        assert_eq!(code, 0, "{report}");
        let home_check = check(&report, "home");
        assert_eq!(home_check["level"], "warn");
        assert!(home_check["detail"].as_str().unwrap().contains("mode 0755"));
        assert_eq!(
            home_check["fix"].as_str().unwrap(),
            format!("chmod 700 {}", state.display())
        );
        set_mode(&state, 0o700);
    }

    // A warn alone does not fail the run: no service unit, no AI accounts.
    {
        std::fs::remove_file(&unit).unwrap();
        let (code, report) = doctor(&home, &state, &[]);
        assert_eq!(code, 0, "{report}");
        let service = check(&report, "service");
        assert_eq!(service["level"], "warn");
        assert!(service["fix"]
            .as_str()
            .unwrap()
            .contains("idlemax service install"));
        assert_eq!(level(&report, "ai"), "ok");
    }

    // The text report prints one line per check and the fix under the broken one.
    let out = Command::new(BIN)
        .args(["--format", "table", "doctor"])
        .env("HOME", &home)
        .env("IDLEMAX_HOME", &state)
        .env_remove("IDLEMAX_TOKEN")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("warn  service"), "{text}");
    assert!(
        text.contains("fix: run `idlemax service install`"),
        "{text}"
    );
    assert!(text.contains("12 checks: 11 ok, 1 warn, 0 fail"), "{text}");
}

#[test]
fn doctor_checks_claude_login() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let state = temp.path().join("state");
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    set_mode(&state, 0o700);

    let coord_port = free_port();
    let agent_port = free_port();
    let claude_bin = bin_dir.join("claude");

    // Case 1: with no account the report holds no login. check
    write_config(&state, coord_port, agent_port, NO_DISK_GUARD, "[]");
    let (_, report) = doctor(&home, &state, &[]);
    let has_login = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"].as_str().unwrap().starts_with("login."));
    assert!(
        !has_login,
        "no login check expected when no account configured"
    );

    // Append one claude account to config
    let config_path = state.join("config.toml");
    let mut config_text = std::fs::read_to_string(&config_path).unwrap();
    config_text.push_str("\n\n[[quota.accounts]]\nname = \"claude-main\"\nprovider = \"claude\"\n");
    std::fs::write(&config_path, &config_text).unwrap();

    // System PATH + bin_dir first
    let current_path = std::env::var("PATH").unwrap_or_default();
    let test_path = format!("{}:{current_path}", bin_dir.display());

    // Case 2: stub claude prints {"loggedIn": false}
    std::fs::write(&claude_bin, "#!/bin/sh\necho '{\"loggedIn\": false}'\n").unwrap();
    set_mode(&claude_bin, 0o755);

    let (_, report_logged_out) = doctor_with_path(&home, &state, &test_path);
    let check_logged_out = check(&report_logged_out, "login.claude");
    assert_eq!(check_logged_out["level"], "warn");
    let detail = check_logged_out["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("needs you:"),
        "detail should start with 'needs you:', got: {detail}"
    );

    // Case 3: stub prints other text
    std::fs::write(&claude_bin, "#!/bin/sh\necho 'internal server error 500'\n").unwrap();
    set_mode(&claude_bin, 0o755);

    let (_, report_other) = doctor_with_path(&home, &state, &test_path);
    let check_other = check(&report_other, "login.claude");
    assert_eq!(check_other["level"], "warn");
    let detail_other = check_other["detail"].as_str().unwrap();
    assert!(
        detail_other.starts_with("cannot tell whether"),
        "detail should start with 'cannot tell whether', got: {detail_other}"
    );
}
