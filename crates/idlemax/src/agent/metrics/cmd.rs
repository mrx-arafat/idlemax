//! Adapter: runs a short probe command with a hard timeout so a stuck tool never stalls the agent.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Default per-probe timeout. Every probe used here returns in well under 100 ms.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Runs `prog args`, returns stdout when the process exits before `timeout`.
/// Output is returned even on a non-zero exit (sysctl exits 1 on one unknown oid
/// but still prints the others). Returns None when it cannot spawn or times out;
/// a timed out child is killed and reaped.
pub fn run(prog: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(prog)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    // Drain stdout concurrently so a chatty child never blocks on a full pipe.
    thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    // A grandchild could keep the pipe open; never wait on it for long.
    rx.recv_timeout(Duration::from_millis(500)).ok()
}
