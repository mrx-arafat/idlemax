//! Integration tests for `idlemax logs` and `idlemax inbox` CLI commands.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use idlemax_core::job_run::inbox::{self, RunReport};
use idlemax_core::job_run::Outcome;
use idlemax_core::types::{CostClass, Job, JobId, JobSpec, JobStatus};
use idlemax_core::Paths;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

struct TempHome {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    paths: Paths,
}

impl TempHome {
    fn new() -> Self {
        Self::new_with_coord(7878)
    }

    fn new_with_coord(port: u16) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let state = tmp.path().join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        let paths = Paths::new(&state);
        for dir in paths.dirs() {
            fs::create_dir_all(dir).unwrap();
        }

        let config = format!(
            "[coordinator]\nlisten = \"127.0.0.1:{port}\"\nurl = \"http://127.0.0.1:{port}\"\n"
        );
        fs::write(paths.config_file(), config).unwrap();
        fs::write(
            paths.token_file(),
            "testtoken-0123456789abcdef-0123456789\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(paths.token_file(), fs::Permissions::from_mode(0o600)).unwrap();
        }

        Self {
            _tmp: tmp,
            home,
            state,
            paths,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .args(args)
            .env("HOME", &self.home)
            .env("IDLEMAX_HOME", &self.state)
            .output()
            .unwrap()
    }
}

fn write_fixture_log(paths: &Paths, id: JobId, lines: &[&str]) {
    let path = paths.job_log_file(id);
    let mut text = lines.join("\n");
    text.push_str("\n=== idlemax: succeeded (exit 0) ===\n");
    fs::write(path, text).unwrap();
}

fn write_fixture_note(paths: &Paths, id: JobId, name: &str, status: JobStatus, output: &str) {
    let job = Job {
        id,
        spec: JobSpec {
            name: name.into(),
            command: vec!["/bin/echo".into(), output.into()],
            cost: CostClass::CpuLight,
            ..JobSpec::default()
        },
        attempts: 1,
        ..Job::default()
    };
    let outcome = Outcome {
        status,
        exit_code: Some(0),
        error: None,
    };
    let report = RunReport {
        job: &job,
        device: "laptop",
        outcome: &outcome,
        started_ms: 1730000000000,
        finished_ms: 1730000001000,
        active_ms: 1000,
        wrapper: "taskpolicy",
        log_path: "/tmp/log",
        output,
        utc_offset_secs: 0,
    };
    let note_content = inbox::render(&report);
    let fname = inbox::file_name(id, name, report.finished_ms, 0);
    fs::write(paths.inbox_dir().join(fname), note_content).unwrap();
}

#[test]
fn logs_fixture_full_and_tail() {
    let env = TempHome::new();
    write_fixture_log(&env.paths, 10, &["line one", "line two", "line three"]);

    // Full logs
    let out = env.run(&["logs", "10"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("line one"));
    assert!(stdout.contains("line two"));
    assert!(stdout.contains("line three"));
    assert!(stdout.contains("=== idlemax: succeeded (exit 0) ==="));

    // Tail logs (last 2 lines)
    let out_tail = env.run(&["logs", "10", "--tail", "2"]);
    assert!(out_tail.status.success());
    let stdout_tail = String::from_utf8_lossy(&out_tail.stdout);
    assert!(!stdout_tail.contains("line one"));
    assert!(stdout_tail.contains("line three"));
    assert!(stdout_tail.contains("=== idlemax: succeeded (exit 0) ==="));
}

#[test]
fn inbox_fixture_list_and_show() {
    let env = TempHome::new();
    write_fixture_note(
        &env.paths,
        21,
        "backup-job",
        JobStatus::Succeeded,
        "backup complete",
    );

    // List table
    let out_list = env.run(&["inbox"]);
    assert!(out_list.status.success());
    let list_str = String::from_utf8_lossy(&out_list.stdout);
    assert!(list_str.contains("21"));
    assert!(list_str.contains("backup-job"));
    assert!(list_str.contains("succeeded"));

    // List JSON
    let out_json = env.run(&["inbox", "list", "--json"]);
    assert!(out_json.status.success());
    let json_val: serde_json::Value = serde_json::from_slice(&out_json.stdout).unwrap();
    assert!(json_val.is_array());
    let arr = json_val.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["job_id"], 21);
    assert_eq!(arr[0]["name"], "backup-job");
    assert_eq!(arr[0]["result"], "succeeded");

    // Show note
    let out_show = env.run(&["inbox", "show", "21"]);
    assert!(out_show.status.success());
    let show_str = String::from_utf8_lossy(&out_show.stdout);
    assert!(show_str.contains("# backup-job: succeeded"));
    assert!(show_str.contains("backup complete"));

    // Show nonexistent note fails
    let out_none = env.run(&["inbox", "show", "999"]);
    assert!(!out_none.status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_absent_asks_coordinator() {
    use axum::routing::get;
    use axum::{Json, Router};

    let router = Router::new().route(
        "/v1/jobs/12",
        get(|| async {
            Json(serde_json::json!({
                "id": 12,
                "status": "succeeded",
                "device": "server",
                "attempts": 1,
                "created_ms": 1730000000000i64,
                "spec": {
                    "name": "remote-job",
                    "command": ["/bin/echo"],
                    "priority": 0,
                    "cost": "cpu_light",
                    "mem_mb": 64,
                    "max_runtime_secs": 60,
                    "max_attempts": 1,
                }
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let env = TempHome::new_with_coord(port);
    let out = env.run(&["--format", "table", "logs", "12"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "job 12 ran on server; run this there");

    // Piped, the same answer is TOON an agent can act on.
    let out = env.run(&["logs", "12"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("job: 12\nstatus: succeeded\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Run `idlemax logs 12` on server"),
        "{stdout}"
    );
}
