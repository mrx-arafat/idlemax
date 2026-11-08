//! Tests: runner pause, kill and memory cap against real processes (unix only).

use std::path::Path;
use std::time::{Duration, Instant};

use idlemax_core::config::AgentConfig;
use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{Job, JobSpec, JobStatus};
use idlemax_core::Paths;

use super::os::{self, Sig};
use super::Runner;

#[test]
fn sleeping_past_runtime_survives_until_awake_limit() {
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    let mut task = job(9, &["sh", "-c", "while :; do sleep 1; done"], 64);
    task.spec.max_runtime_secs = 10;
    let wall = crate::state::now_ms();
    r.start_at(&task, wall, 0).unwrap();
    // Inject a clock with one hour of sleep and just one second awake.
    let wall_slept = wall + 3_600_000;
    r.poll_at(wall_slept, 1_000);
    assert!(r.slots[&9].stop.is_none(), "sleep consumed the runtime cap");
    r.poll_at(wall_slept + 9_001, 10_001);
    assert!(matches!(
        r.slots[&9].stop.as_ref().map(|s| &s.reason),
        Some(idlemax_core::job_run::StopReason::MaxRuntime { limit_secs: 10 })
    ));
    let update = wait_ended(&mut r, STARVED);
    assert_eq!(update.status, JobStatus::Killed);
    assert_eq!(
        update.error.as_deref(),
        Some("max runtime of 10 s exceeded")
    );
}

fn runner(home: &Path, allow: &[&str]) -> Runner {
    let config = AgentConfig {
        command_allowlist: allow.iter().map(|s| s.to_string()).collect(),
        ..AgentConfig::default()
    };
    Runner::new(Paths::new(home), "test-device".into(), config)
}

fn job(id: i64, argv: &[&str], mem_mb: u64) -> Job {
    let spec = JobSpec {
        name: format!("test job {id}"),
        command: argv.iter().map(|s| s.to_string()).collect(),
        mem_mb,
        ..JobSpec::default()
    };
    Job {
        id,
        attempts: 1,
        spec,
        ..Job::default()
    }
}

/// Jobs run at background priority (`taskpolicy -c background`), so on a busy machine they
/// are starved on purpose: starting, stopping or dying takes seconds, not milliseconds.
/// Waits return the moment the condition holds, so a generous limit costs nothing when green.
pub(super) const STARVED: Duration = Duration::from_secs(60);

/// A memory-cap kill, whoever enforced it: the runner's poll ("N MiB over the C MiB cap") or,
/// when the job runs in a systemd scope, the kernel ("killed by the C MiB memory cap").
pub(super) fn is_cap_kill(error: &str, cap_mib: u64) -> bool {
    error.ends_with(&format!("MiB over the {cap_mib} MiB cap"))
        || error == format!("killed by the {cap_mib} MiB memory cap")
}

/// Polls `cond` until it holds; panics naming `what` after `within`. No fixed sleeps.
pub(super) fn wait_until(what: &str, within: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_ended(r: &mut Runner, within: Duration) -> JobUpdate {
    let deadline = Instant::now() + within;
    loop {
        if let Some(update) = r.poll().into_iter().next() {
            return update;
        }
        assert!(
            Instant::now() < deadline,
            "job did not end in time; running: {:?}",
            r.running()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn stdin_reaches_the_job() {
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    let mut job = job(2, &["sh", "-c", "cat"], 64);
    job.spec.stdin = Some("private prompt sentinel".into());
    r.start(&job).unwrap();

    let update = wait_ended(&mut r, STARVED);
    assert_eq!(update.status, JobStatus::Succeeded);
    assert_eq!(
        update.output_tail.as_deref(),
        Some("private prompt sentinel")
    );
}

/// A program that never reads its input must not hold up the agent: `start` runs on the
/// agent loop under the runner lock, so a blocked write there stops the guard and the kill switch.
#[test]
fn a_job_that_ignores_stdin_does_not_block_start_and_one_that_reads_gets_every_byte() {
    const MIB: usize = 1024 * 1024;
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);

    let mut idle = job(3, &["sh", "-c", "sleep 30"], 64);
    idle.spec.stdin = Some("x".repeat(MIB));
    let began = Instant::now();
    r.start(&idle).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "start waited {:?} for a job that does not read stdin",
        began.elapsed()
    );
    r.kill(3).unwrap();
    wait_ended(&mut r, STARVED);

    let mut reader = job(4, &["sh", "-c", "wc -c"], 64);
    reader.spec.stdin = Some("y".repeat(MIB));
    r.start(&reader).unwrap();
    let update = wait_ended(&mut r, STARVED);
    assert_eq!(update.status, JobStatus::Succeeded);
    assert_eq!(
        update.output_tail.as_deref().map(str::trim),
        Some("1048576")
    );
}

/// True once the OS reports `pid` as stopped (SIGSTOP has taken effect).
fn stopped(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
    let (mut sys, pid) = (System::new().unwrap(), Pid::from_u32(pid));
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .is_some_and(|p| p.status() == ProcessStatus::Stop)
}

#[test]
fn pause_freezes_the_group_kill_ends_it_and_the_memory_cap_kills_the_tree() {
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    r.start(&job(
        3,
        &["sh", "-c", "while :; do echo tick; sleep 0.05; done"],
        64,
    ))
    .unwrap();
    let root = r.running()[0].pid.unwrap();
    let log = Paths::new(home.path()).job_log_file(3);
    let len = || std::fs::metadata(&log).map_or(0, |m| m.len());
    wait_until("the job to write", STARVED, || len() > 0);
    r.pause(3).unwrap();
    // SIGSTOP lands when the job is next scheduled: wait for the OS to say so, then it
    // must stay silent. A stopped job cannot write, however busy the machine is.
    wait_until("the paused job to stop", STARVED, || stopped(root));
    let frozen = len();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(len(), frozen, "paused job kept writing");
    assert_eq!(r.running()[0].status, JobStatus::Paused);
    r.resume(3).unwrap();
    wait_until("the resumed job to write", STARVED, || len() > frozen);
    r.pause(3).unwrap();
    r.kill(3).unwrap();
    let u = wait_ended(&mut r, STARVED);
    assert_eq!((u.status, u.error), (JobStatus::Killed, None));

    // `; echo` keeps sh as the parent, so the hog is a grandchild of the job root.
    // Random data, every page touched every 50 ms: macOS compresses or swaps idle pages out
    // of the resident size the cap reads.
    let hog = r#"$b = pack("L*", map { int rand 4294967296 } 1 .. 16384); $x = $b x 1536; while (1) { vec($x, $_ * 4096, 8) ^= 1 for 0 .. 24575; select(undef, undef, undef, 0.05) }"#;
    let script = format!("perl -e '{hog}'; echo done");
    r.start(&job(4, &["sh", "-c", &script], 32)).unwrap();
    let u = wait_ended(&mut r, STARVED);
    assert_eq!(u.status, JobStatus::Killed, "{u:?}");
    let error = u.error.unwrap_or_default();
    assert!(is_cap_kill(&error, 32), "{error}");
}

#[test]
fn signalling_a_group_of_unreaped_zombies_is_not_an_error() {
    use std::os::unix::process::CommandExt;
    // macOS answers EPERM, not ESRCH, while every process left in a group is a zombie.
    let mut root = std::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = root.id();
    wait_until("the group leader to exit", STARVED, || !os::alive(pgid));
    os::signal_group(pgid, Sig::Term).unwrap();
    os::signal_group(pgid, Sig::Kill).unwrap();
    root.wait().unwrap();
}
