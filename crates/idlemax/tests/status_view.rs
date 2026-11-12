//! `idlemax status` through the real binary against stub coordinators: one that predates
//! `GET /v1/dispatcher` (404) and one whose view fails (500). Either way the status prints and
//! exits 0, because the dispatcher block is best effort.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use idlemax_core::Paths;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    url
}

fn status(url: &str) -> std::process::Output {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let paths = Paths::new(&state);
    for dir in paths.dirs() {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(
        paths.token_file(),
        "testtoken-0123456789abcdef-0123456789\n",
    )
    .unwrap();
    fs::set_permissions(paths.token_file(), fs::Permissions::from_mode(0o600)).unwrap();
    Command::new(BIN)
        .args(["--coord", url, "--format", "table", "status"])
        .env_clear()
        .env("HOME", tmp.path())
        .env("IDLEMAX_HOME", &state)
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn status_prints_without_a_dispatcher_view_and_when_the_view_fails() {
    let empty = || get(|| async { Json(serde_json::json!({})) });
    let old = serve(Router::new().route("/v1/status", empty())).await;
    let out = tokio::task::spawn_blocking(move || status(&old))
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("no devices have sent a heartbeat yet"),
        "{text}"
    );
    assert!(!text.contains("dispatcher"), "{text}");
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let failing = Router::new().route("/v1/status", empty()).route(
        "/v1/dispatcher",
        get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let broken = serve(failing).await;
    let out = tokio::task::spawn_blocking(move || status(&broken))
        .await
        .unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("dispatcher:"));
    let note = String::from_utf8_lossy(&out.stderr);
    assert!(
        note.contains("the dispatcher view is not available"),
        "{note}"
    );
}
