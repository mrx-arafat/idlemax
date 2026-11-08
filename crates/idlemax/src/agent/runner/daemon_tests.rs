//! Tests: a job that daemonizes a memory hog is still capped, and the hog dies with it
//! (unix only). The job, the helper that exits and the hog are this test binary re-run in a role.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use idlemax_core::config::AgentConfig;
use idlemax_core::types::{Job, JobSpec, JobStatus};
use idlemax_core::Paths;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use super::os;
use super::tests::{is_cap_kill, wait_until, STARVED};
use super::Runner;

const ROLE: &str = "IDLEMAX_TEST_ROLE";
const HOG_PID_FILE: &str = "IDLEMAX_TEST_HOG_PID_FILE";
const NEW_GROUP: &str = "IDLEMAX_TEST_NEW_GROUP";
const HOG_MIB: usize = 96;
/// Each test has its own job id, and no other runner test uses these: a live runner adopts
/// (and a restart reaps) every process tagged with its job id, whoever started it.
const CAP_JOB: i64 = 51;
const RESTART_JOB: i64 = 52;

/// Not a test by itself: the roles below re-run this binary and land here.
#[test]
fn role_entry() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    match role.as_str() {
        // The job stays alive and starts a helper that will exit.
        "job" => {
            spawn("helper", false);
            std::thread::sleep(Duration::from_secs(60));
        }
        // The helper starts the hog and exits, so the hog is reparented to init.
        "helper" => spawn("hog", std::env::var(NEW_GROUP).is_ok()),
        _ => {
            let pid_file = std::env::var(HOG_PID_FILE).unwrap_or_default();
            std::fs::write(pid_file, std::process::id().to_string()).unwrap();
            // macOS compresses idle or uniform pages out of the resident size, so the hog
            // holds incompressible data and touches every page: it stays resident.
            let mut held = incompressible(HOG_MIB * 1024 * 1024);
            loop {
                held.iter_mut()
                    .step_by(4096)
                    .for_each(|b| *b = b.wrapping_add(1));
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// `len` bytes the OS cannot squeeze: one random block (xorshift) repeated. Compression
/// works per page, so repeating a block larger than a page stays incompressible and is cheap.
fn incompressible(len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let block: Vec<u8> = (0..8192)
        .flat_map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()
        })
        .collect();
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
        let take = (len - bytes.len()).min(block.len());
        bytes.extend_from_slice(&block[..take]);
    }
    bytes
}

// The child is left unwaited on purpose: the hog must outlive its parent.
#[allow(clippy::zombie_processes)]
fn spawn(role: &str, new_group: bool) {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["role_entry", "--nocapture"])
        .env(ROLE, role)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if new_group {
        cmd.process_group(0);
    }
    cmd.spawn().unwrap();
}

fn runner(home: &Path, exe: &str) -> Runner {
    let config = AgentConfig {
        command_allowlist: vec![exe.to_string()],
        ..AgentConfig::default()
    };
    Runner::new(Paths::new(home), "test-device".into(), config)
}

fn job(id: i64, exe: &str, cwd: &Path, env: &[(&str, String)], mem_mb: u64) -> Job {
    let spec = JobSpec {
        name: "daemonizing job".into(),
        command: vec![exe.into(), "role_entry".into(), "--nocapture".into()],
        mem_mb,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        ..JobSpec::default()
    };
    Job {
        id,
        attempts: 1,
        spec,
        ..Job::default()
    }
}

/// The process ends within `STARVED`.
fn gone(pid: u32) -> bool {
    let until = Instant::now() + STARVED;
    while os::alive(pid) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    !os::alive(pid)
}

/// The hog's pid once it has finished writing its pid file (the file exists before its content).
fn read_pid(pid_file: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_file).ok()?.trim().parse().ok()
}

type Sample = (u128, Option<u64>, Option<u32>, Option<u64>);

/// What the runner measured for the job next to the hog's own resident size.
fn sample(start: Instant, r: &Runner, pid_file: &Path) -> Sample {
    let mib = |bytes: u64| bytes / (1024 * 1024);
    let seen = r.running().first().and_then(|j| j.rss_bytes).map(mib);
    let hog = read_pid(pid_file);
    let own = hog.and_then(|pid| {
        let (mut sys, pid) = (System::new().ok()?, Pid::from_u32(pid));
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        sys.process(pid).map(|p| mib(p.memory()))
    });
    (start.elapsed().as_millis(), seen, hog, own)
}

#[test]
fn a_daemonized_hog_counts_against_the_cap_and_is_killed_with_the_job() {
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // Same group, reparented to init; then a hog that also left the job's process group.
    for new_group in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let pid_file = home.path().join("hog.pid");
        let mut env = vec![
            (ROLE, "job".to_string()),
            (HOG_PID_FILE, pid_file.to_string_lossy().into_owned()),
        ];
        if new_group {
            env.push((NEW_GROUP, "1".into()));
        }
        let mut r = runner(home.path(), &exe);
        r.start(&job(CAP_JOB, &exe, home.path(), &env, 64)).unwrap();

        let (start, deadline) = (Instant::now(), Instant::now() + STARVED);
        let mut trace = Vec::new();
        let update = loop {
            if let Some(u) = r.poll().into_iter().next() {
                break u;
            }
            trace.push(sample(start, &r, &pid_file));
            assert!(
                Instant::now() < deadline,
                "new_group={new_group}: not killed; (ms, runner MiB, hog pid, hog MiB): {:?}",
                trace.iter().step_by(20).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let error = update.error.clone().unwrap_or_default();
        assert_eq!(update.status, JobStatus::Killed, "{update:?}");
        assert!(is_cap_kill(&error, 64), "{error}");

        let hog = read_pid(&pid_file).expect("the hog wrote its pid before it was capped");
        assert!(
            gone(hog),
            "new_group={new_group}: hog {hog} outlived the job"
        );
    }
}

#[test]
fn a_restarted_agent_kills_daemonized_members_of_orphaned_jobs() {
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // The job root survived the crash, or died and left only the hog that left its group.
    for root_alive in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let pid_file = home.path().join("hog.pid");
        let env = [
            (ROLE, "job".to_string()),
            (HOG_PID_FILE, pid_file.to_string_lossy().into_owned()),
            (NEW_GROUP, "1".to_string()),
        ];
        let mut crashed = runner(home.path(), &exe);
        crashed
            .start(&job(RESTART_JOB, &exe, home.path(), &env, 4096))
            .unwrap();
        wait_until("the hog's pid file", STARVED, || {
            read_pid(&pid_file).is_some()
        });
        let hog = read_pid(&pid_file).unwrap();
        let root = crashed.running()[0].pid.unwrap();
        if !root_alive {
            let child = &mut crashed.slots.get_mut(&RESTART_JOB).unwrap().child;
            child.kill().unwrap();
            child.wait().unwrap();
        }
        // The agent died: nothing ran its cleanup, and the pid file is still there.
        std::mem::forget(crashed);

        let mut restarted = runner(home.path(), &exe);
        let updates = restarted.poll();
        assert!(
            gone(hog),
            "root_alive={root_alive}: hog {hog} survived the restart"
        );
        assert!(gone(root), "root_alive={root_alive}: root {root} survived");
        let errors: Vec<_> = updates
            .iter()
            .map(|u| (u.status, u.error.clone()))
            .collect();
        let killed = (
            JobStatus::Killed,
            Some("agent restarted; orphaned job killed".into()),
        );
        assert_eq!(errors, if root_alive { vec![killed] } else { vec![] });
    }
}
