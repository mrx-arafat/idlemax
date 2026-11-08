//! CLI recovery messages must identify the action that repairs the actual failure.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const STATE: &str = "state with spaces";

fn run(root: &Path, args: &[&str], token: Option<&str>) -> Output {
    let state = root.join(STATE);
    assert!(state.starts_with(std::env::temp_dir()));
    std::fs::create_dir_all(&state).unwrap();
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env_clear()
        .env("HOME", root)
        .env("IDLEMAX_HOME", state);
    if let Some(token) = token {
        command.env("IDLEMAX_TOKEN", token);
    }
    command.output().unwrap()
}

#[test]
fn token_errors_name_the_actual_problem_and_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(run(tmp.path(), &["init"], None).status.success());
    let token = tmp.path().join(STATE).join("token");
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = run(tmp.path(), &["status", "--json"], None);
    assert_eq!(out.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(value["error"].as_str().unwrap().contains("mode 0600"));
    assert_eq!(value["help"], format!("chmod 600 '{}'", token.display()));

    let out = run(tmp.path(), &["status", "--json"], Some("short"));
    assert_eq!(out.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let message = value["error"].as_str().unwrap();
    assert!(message.contains("IDLEMAX_TOKEN"), "{value}");
    assert!(
        message.contains("too short") && message.contains("at least 32"),
        "{value}"
    );
    assert!(!message.contains("set but empty"), "{value}");
    assert!(value["help"]
        .as_str()
        .unwrap()
        .contains("unset IDLEMAX_TOKEN"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_reports_a_known_allowlist_blocker_on_stdout_without_timing_out() {
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use idlemax_core::protocol::{ClusterStatus, DeviceView};
    use idlemax_core::types::{DeviceInfo, Job, JobSpec};

    let job = Job {
        id: 7,
        spec: JobSpec {
            name: "sleep".into(),
            command: vec!["/bin/sleep".into(), "0".into()],
            ..JobSpec::default()
        },
        ..Job::default()
    };
    let queued = job.clone();
    let status = ClusterStatus {
        devices: vec![DeviceView {
            info: DeviceInfo {
                id: "ux-walk".into(),
                command_allowlist: Some(vec!["/bin/echo".into()]),
                ..DeviceInfo::default()
            },
            ..DeviceView::default()
        }],
        ..ClusterStatus::default()
    };
    let app = Router::new()
        .route("/v1/jobs", post(move || async move { Json(queued) }))
        .route("/v1/jobs/7", get(move || async move { Json(job) }))
        .route("/v1/status", get(move || async move { Json(status) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join(STATE);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("config.toml"),
        format!("[coordinator]\nurl = 'http://127.0.0.1:{port}'\n"),
    )
    .unwrap();
    for format in ["toon", "json"] {
        let out = run(
            tmp.path(),
            &[
                "job",
                "add",
                "--wait",
                "--timeout",
                "1",
                "--format",
                format,
                "--",
                "/bin/sleep",
                "0",
            ],
            Some("0123456789abcdef0123456789abcdef"),
        );
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("no device allows"), "{text}");
        assert!(text.contains("agent.command_allowlist"), "{text}");
        assert!(text.contains("job 7"), "{text}");
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_an_already_killed_job_is_a_successful_noop_in_every_format() {
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use idlemax_core::protocol::{ClusterStatus, JobCounts};
    use idlemax_core::types::{Job, JobStatus};

    let status = ClusterStatus {
        jobs: JobCounts {
            killed: 1,
            ..JobCounts::default()
        },
        ..ClusterStatus::default()
    };
    let job = Job {
        id: 5,
        status: JobStatus::Killed,
        ..Job::default()
    };
    let app = Router::new()
        .route("/v1/status", get(move || async move { Json(status) }))
        .route("/v1/jobs/5", get(move || async move { Json(job) }))
        .route(
            "/v1/control",
            post(|| async {
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "job 5 already finished"})),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join(STATE);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("config.toml"),
        format!("[coordinator]\nurl = 'http://127.0.0.1:{port}'\n"),
    )
    .unwrap();
    for format in ["table", "toon", "json"] {
        let out = run(
            tmp.path(),
            &["stop-all", "--job", "5", "--format", format],
            Some("0123456789abcdef0123456789abcdef"),
        );
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        if format == "json" {
            let value: ClusterStatus = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(value.jobs.killed, 1);
        } else {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(text.contains("already killed (no-op)"), "{text}");
            assert!(!text.contains("resume --job 5"), "{text}");
            if format == "toon" {
                assert!(text.contains("idlemax jobs"), "{text}");
            }
        }
    }
    server.abort();
}
