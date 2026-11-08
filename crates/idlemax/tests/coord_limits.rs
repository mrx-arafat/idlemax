//! A flood of idle connections must not take the coordinator down: when the process runs out
//! of file descriptors `accept` fails, and the server has to log that and keep serving.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");

/// A child killed on drop.
struct Coordinator(Child);

impl Drop for Coordinator {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn idlemax(args: &[&str], home: &Path, state: &Path) {
    assert!(
        state.starts_with(std::env::temp_dir()),
        "state must be a temp dir"
    );
    let out = Command::new(BIN)
        .args(args)
        .env("HOME", home)
        .env("IDLEMAX_HOME", state)
        .env_remove("IDLEMAX_TOKEN")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn healthy(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let request = "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let mut reply = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut reply).is_ok()
        && reply.starts_with("HTTP/1.1 200")
}

#[test]
fn running_out_of_file_descriptors_does_not_stop_the_coordinator() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, state) = (tmp.path().join("home"), tmp.path().join("state"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let port = free_port();
    std::fs::write(
        state.join("config.toml"),
        format!(
            "[coordinator]\nlisten = \"127.0.0.1:{port}\"\nurl = \"http://127.0.0.1:{port}\"\n"
        ),
    )
    .unwrap();
    idlemax(&["init"], &home, &state);

    // 48 descriptors: enough to start, few enough for a modest flood to use them all up.
    let child = Command::new("sh")
        .args(["-c", "ulimit -n 48 && exec \"$0\" coord", BIN])
        .env("HOME", &home)
        .env("IDLEMAX_HOME", &state)
        .env_remove("IDLEMAX_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut coord = Coordinator(child);
    let start = Instant::now();
    while !healthy(port) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "coordinator did not start"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Idle connections until the server has no descriptor left to accept with.
    let flood: Vec<TcpStream> = (0..80)
        .filter_map(|_| TcpStream::connect(("127.0.0.1", port)).ok())
        .collect();
    std::thread::sleep(Duration::from_millis(800));
    drop(flood);

    assert!(
        coord.0.try_wait().unwrap().is_none(),
        "the coordinator exited when it ran out of descriptors"
    );
    let start = Instant::now();
    while !healthy(port) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the coordinator stopped answering after the flood"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
