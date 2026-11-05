//! Tests: runner pause, kill and memory cap against real processes (unix only).

use std::path::Path;
use std::time::{Duration, Instant};

use idlemax_core::config::AgentConfig;
use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{Job, JobSpec, JobStatus};
use idlemax_core::Paths;

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
    let update = wait_ended(&mut r, Duration::from_secs(5));
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

fn wait_ended(r: &mut Runner, within: Duration) -> JobUpdate {
    let deadline = Instant::now() + within;
    loop {
        if let Some(update) = r.poll().into_iter().next() {
            return update;
        }
        assert!(Instant::now() < deadline, "job did not end in time");
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

    let update = wait_ended(&mut r, Duration::from_secs(10));
    assert_eq!(update.status, JobStatus::Succeeded);
    assert_eq!(
        update.output_tail.as_deref(),
        Some("private prompt sentinel")
    );
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
    let log = Paths::new(home.path()).job_log_file(3);
    let len = || std::fs::metadata(&log).unwrap().len();
    std::thread::sleep(Duration::from_millis(300));
    r.pause(3).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let frozen = len();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(len(), frozen, "paused job kept writing");
    assert_eq!(r.running()[0].status, JobStatus::Paused);
    r.resume(3).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(len() > frozen, "resumed job did not continue");
    r.pause(3).unwrap();
    r.kill(3).unwrap();
    let u = wait_ended(&mut r, Duration::from_secs(10));
    assert_eq!((u.status, u.error), (JobStatus::Killed, None));

    // `; echo` keeps sh as the parent, so the hog is a grandchild of the job root.
    let script = "perl -e '$x = \"x\" x (96 * 1024 * 1024); sleep 60'; echo done";
    r.start(&job(4, &["sh", "-c", script], 32)).unwrap();
    let u = wait_ended(&mut r, Duration::from_secs(20));
    assert_eq!(u.status, JobStatus::Killed, "{u:?}");
    let error = u.error.unwrap_or_default();
    assert!(error.ends_with("MiB over the 32 MiB cap"), "{error}");
}
