//! `idlemax mcp` through the real binary over a pipe: the handshake and the tool list need no
//! coordinator, `status` shows a seeded worker's label, the lock refuses an on-switch before
//! any network call, and a home without a token says to run `idlemax setup`.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
const DEMO: &str = include_str!("fixtures/dispatcher_demo.sql");

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn idlemax(dir: &Path) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir.join("home"))
        .env("IDLEMAX_HOME", dir.join("state"));
    cmd
}

/// Sends `lines` to `idlemax mcp` and returns every stdout line, each parsed as JSON.
fn session(mut cmd: Command, lines: &[Value]) -> Vec<Value> {
    let mut child = cmd
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for line in lines {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin);
    let out = BufReader::new(child.stdout.take().unwrap());
    let replies = out
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).expect("every stdout line is JSON"))
        .collect();
    assert!(child.wait().unwrap().success());
    replies
}

fn handshake(version: &str) -> Vec<Value> {
    vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": version, "capabilities": {},
                          "clientInfo": {"name": "test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ]
}

fn call(id: i64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
           "params": {"name": name, "arguments": arguments}})
}

fn text(reply: &Value) -> (bool, String) {
    let r = &reply["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or_default();
    (r["isError"].as_bool().unwrap_or(false), text.to_string())
}

/// A coordinator in `dir` on a spare port with the demo rows loaded; killed on drop.
struct Coord(Child);

impl Drop for Coord {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn seeded_coord(dir: &Path) -> Coord {
    let state = dir.join("state");
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let port = free_port();
    std::fs::write(
        state.join("config.toml"),
        format!(
            "[coordinator]\nlisten = \"127.0.0.1:{port}\"\nurl = \"http://127.0.0.1:{port}\"\n\n\
             [advisor]\nenabled = false\n"
        ),
    )
    .unwrap();
    assert!(idlemax(dir).arg("init").status().unwrap().success());
    let coord = Coord(
        idlemax(dir)
            .arg("coord")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "coordinator never came up"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let db = rusqlite::Connection::open(state.join("coord.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(DEMO).unwrap();
    coord
}

#[test]
fn the_server_answers_the_handshake_and_status_shows_the_workers() {
    let dir = tempfile::tempdir().unwrap();
    let _coord = seeded_coord(dir.path());
    let mut lines = handshake("2025-03-26");
    lines.push(call(3, "status", json!({})));
    lines.push(call(4, "finished_work", json!({})));
    let replies = session(idlemax(dir.path()), &lines);
    assert_eq!(replies.len(), 4, "{replies:?}");
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 6);
    let (failed, status) = text(&replies[2]);
    assert!(!failed && status.contains("working"), "{status}");
    let (failed, review) = text(&replies[3]);
    assert!(!failed && review.contains("ready for review"), "{review}");
}

#[test]
fn a_launched_server_refuses_on_switches_and_one_without_a_token_says_setup() {
    let dir = tempfile::tempdir().unwrap();
    let closed = format!("http://127.0.0.1:{}", free_port());
    let mut marked = idlemax(dir.path());
    marked.args(["--coord", &closed]).env("IDLEMAX_JOB_ID", "7");
    let replies = session(
        marked,
        &[
            call(1, "start", json!({})),
            call(2, "project_on", json!({"name": "app"})),
        ],
    );
    for reply in &replies {
        let (failed, said) = text(reply);
        assert!(failed, "{said}");
        assert!(
            said.contains("a job or worker that idlemax started"),
            "{said}"
        );
    }

    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    let mut lines = handshake("2025-06-18");
    lines.push(call(3, "status", json!({})));
    let replies = session(idlemax(dir.path()), &lines);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "idlemax");
    let (failed, said) = text(&replies[2]);
    assert!(failed && said.contains("idlemax setup"), "{said}");
}
