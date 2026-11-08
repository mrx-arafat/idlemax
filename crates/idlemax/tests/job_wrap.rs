//! The job supervisor as the real binary: a stop sent to the whole process group must still
//! reach the job, and the job's exit must be recorded and passed on unchanged.
#![cfg(unix)]

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use idlemax_core::job_run::ExitMarker;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;

const BIN: &str = env!("CARGO_BIN_EXE_idlemax");
/// Generous: the job is a background process on a machine that may be busy.
const LIMIT: Duration = Duration::from_secs(60);

/// Runs `idlemax job-wrap` on `script` in its own process group, like the agent does.
fn supervise(dir: &Path, script: &str) -> Child {
    Command::new(BIN)
        .arg("job-wrap")
        .arg("--marker")
        .arg(dir.join("m.exit"))
        .args(["--", "/bin/sh", "-c", script])
        .env("HOME", dir)
        .env("IDLEMAX_HOME", dir.join("state"))
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(dir.join("out.txt")).unwrap())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap()
}

/// Polls `cond`; on timeout kills the supervisor's group so no job outlives a failed test.
fn wait_until(what: &str, group: u32, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + LIMIT;
    while !cond() {
        if Instant::now() > deadline {
            let _ = killpg(Pid::from_raw(group as i32), Signal::SIGKILL);
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn finish(child: &mut Child) -> ExitStatus {
    let mut status = None;
    wait_until("the supervisor to exit", child.id(), || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    status.unwrap()
}

fn marker(dir: &Path) -> ExitMarker {
    let text = std::fs::read_to_string(dir.join("m.exit")).expect("the exit marker");
    ExitMarker::parse(&text).expect("a readable marker")
}

#[test]
fn a_stop_sent_to_the_group_reaches_the_job_and_its_exit_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let script = r#"trap 'echo got-term; exit 3' TERM; echo ready; while :; do sleep 1; done"#;
    let mut child = supervise(dir.path(), script);
    let out = dir.path().join("out.txt");
    wait_until("the job to be ready", child.id(), || {
        std::fs::read_to_string(&out).is_ok_and(|t| t.contains("ready"))
    });

    // The agent stops a job by signalling its group: the supervisor gets the signal too.
    killpg(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let status = finish(&mut child);

    assert!(std::fs::read_to_string(&out).unwrap().contains("got-term"));
    assert_eq!(status.code(), Some(3), "the job's exit code is passed on");
    assert_eq!(marker(dir.path()).code, Some(3));
}

#[test]
fn a_job_killed_by_a_signal_leaves_the_same_signal() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = supervise(dir.path(), "kill -KILL $$");
    let status = finish(&mut child);

    // Not an ordinary exit 137: the agent must still read a signal.
    assert_eq!((status.code(), status.signal()), (None, Some(9)));
    let m = marker(dir.path());
    assert_eq!((m.code, m.signal), (None, Some(9)));
}
