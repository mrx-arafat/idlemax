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

pub(super) fn wait_ended(r: &mut Runner, within: Duration) -> JobUpdate {
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

    let mut idle = job(71, &["sh", "-c", "sleep 30"], 64);
    idle.spec.stdin = Some("x".repeat(MIB));
    let began = Instant::now();
    r.start(&idle).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "start waited {:?} for a job that does not read stdin",
        began.elapsed()
    );
    r.kill(71).unwrap();
    wait_ended(&mut r, STARVED);

    let mut reader = job(72, &["sh", "-c", "wc -c"], 64);
    reader.spec.stdin = Some("y".repeat(MIB));
    r.start(&reader).unwrap();
    let update = wait_ended(&mut r, STARVED);
    assert_eq!(update.status, JobStatus::Succeeded);
    assert_eq!(
        update.output_tail.as_deref().map(str::trim),
        Some("1048576")
    );
}

/// Job output can hold secrets and private repo content: the log and the inbox note are 0600
/// in 0700 directories, so another local user cannot read them.
#[test]
fn job_logs_and_inbox_notes_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    r.start(&job(73, &["sh", "-c", "echo secret output"], 64))
        .unwrap();
    let update = wait_ended(&mut r, STARVED);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let paths = Paths::new(home.path());
    let log = paths.job_log_file(73);
    let note = Path::new(update.output_path.as_deref().expect("a note was written"));
    assert_eq!(mode(&log), 0o600, "job log");
    assert_eq!(mode(note), 0o600, "inbox note");
    assert_eq!(mode(&paths.job_logs_dir()), 0o700, "logs/jobs");
    assert_eq!(mode(&paths.inbox_dir()), 0o700, "inbox");
}

/// `pid state command` of every process in group `pgid`, from `ps`. The state is that of the
/// process's threads: the status the kernel sets when SIGSTOP is sent says "stopped" before a
/// starved process has acted on it.
fn group_states(pgid: u32) -> Vec<String> {
    let out = std::process::Command::new("ps")
        .args(["-axo", "pgid=,pid=,stat=,command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (group, rest) = line.trim_start().split_once(' ')?;
            (group.parse() == Ok(pgid)).then(|| rest.trim().to_string())
        })
        .collect()
}

/// True once every live process of group `pgid` has stopped running.
fn group_stopped(pgid: u32) -> bool {
    let states = group_states(pgid);
    let live: Vec<_> = states
        .iter()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|state| !state.starts_with('Z'))
        .collect();
    !live.is_empty() && live.iter().all(|state| state.starts_with('T'))
}

/// Pause freezes the job's group, resume thaws it, and a kill ends it as `Killed`.
fn pause_resume_and_kill(home: &Path, r: &mut Runner, id: i64) {
    r.start(&job(
        id,
        &["sh", "-c", "while :; do echo tick; sleep 0.05; done"],
        64,
    ))
    .unwrap();
    let root = r.running()[0].pid.unwrap();
    let log = Paths::new(home).job_log_file(id);
    let len = || std::fs::metadata(&log).map_or(0, |m| m.len());
    // The header is written at launch; the first tick proves the job runs its script, past the
    // exec chain (taskpolicy, nice, sh) where a SIGSTOP can be lost.
    wait_until("the job to tick", STARVED, || {
        std::fs::read_to_string(&log).is_ok_and(|t| t.contains("tick"))
    });
    r.pause(id).unwrap();
    // SIGSTOP lands when each process is next scheduled: wait for the OS to say that all of
    // them stopped, then the job must stay silent however busy the machine is.
    let deadline = Instant::now() + STARVED;
    while !group_stopped(root) {
        let states = group_states(root);
        assert!(
            Instant::now() < deadline,
            "the paused group did not stop: {states:#?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let frozen = len();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(len(), frozen, "paused job kept writing");
    assert_eq!(r.running()[0].status, JobStatus::Paused);
    r.resume(id).unwrap();
    wait_until("the resumed job to write", STARVED, || len() > frozen);
    r.pause(id).unwrap();
    r.kill(id).unwrap();
    let u = wait_ended(r, STARVED);
    assert_eq!((u.status, u.error), (JobStatus::Killed, None));
}

/// A job gets the few variables a tool needs to find itself and its login, the spec's own
/// `env` and its id: nothing else from the agent, which may hold API keys or an ssh-agent socket.
#[test]
fn a_job_gets_a_minimal_environment_plus_its_own() {
    // Edition 2021: set_var is safe, and the name is unique to this test.
    std::env::set_var("IDLEMAX_TEST_CANARY_SECRET", "leaked");
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    let mut task = job(6, &["sh", "-c", "env | sort"], 64);
    task.spec.env.insert("FROM_SPEC".into(), "yes".into());
    r.start(&task).unwrap();
    wait_ended(&mut r, STARVED);
    std::env::remove_var("IDLEMAX_TEST_CANARY_SECRET");

    let log = std::fs::read_to_string(Paths::new(home.path()).job_log_file(6)).unwrap();
    // The log's own `=== ... ===` header and footer lines are not variables.
    let names: Vec<&str> = log
        .lines()
        .filter(|l| !l.starts_with("==="))
        .filter_map(|l| l.split_once('='))
        .map(|(k, _)| k)
        .collect();
    for wanted in [
        "PATH",
        "HOME",
        "FROM_SPEC",
        "IDLEMAX_JOB_ID",
        "IDLEMAX_JOB_TAG",
    ] {
        assert!(names.contains(&wanted), "{wanted} missing from:\n{log}");
    }
    assert!(
        !log.contains("IDLEMAX_TEST_CANARY_SECRET"),
        "the agent's variable leaked:\n{log}"
    );
    assert!(
        names.iter().all(|n| idlemax_core::job_run::inherits(n)
            || [
                "FROM_SPEC",
                "IDLEMAX_JOB_ID",
                "IDLEMAX_JOB_TAG",
                "PWD",
                "SHLVL",
                "_",
                "OLDPWD"
            ]
            .contains(n)),
        "unexpected variables in:\n{log}"
    );
}

#[test]
fn pause_freezes_the_group_kill_ends_it_and_the_memory_cap_kills_the_tree() {
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    pause_resume_and_kill(home.path(), &mut r, 3);

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

/// Not a test by itself: a supervised job re-runs this test binary in this role (`supervised`).
#[test]
fn supervisor_role() {
    let args: Vec<_> = std::env::args_os()
        .skip_while(|a| a != "--marker")
        .collect();
    if !args.is_empty() {
        std::process::exit(i32::from(crate::agent::supervise::run(&args)));
    }
}

/// Runs the runner's jobs behind a supervisor, like the agent does. The test binary stands in for
/// `idlemax job-wrap`; libtest keeps everything after its own `--` for the supervisor.
/// It proves the runner's side (files, exit, restart). How the real supervisor treats stop
/// signals is tested on the real binary in `tests/job_wrap.rs`: libtest runs this one on a thread.
fn supervised(r: &mut Runner) {
    let exe = std::env::current_exe().unwrap();
    let mut command = vec![exe.to_string_lossy().into_owned()];
    command.extend(
        [
            "agent::runner::tests::supervisor_role",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "--",
        ]
        .map(String::from),
    );
    r.supervisor = Some(command);
}

#[test]
fn a_supervised_job_pauses_and_stops_like_a_bare_one() {
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    supervised(&mut r);
    pause_resume_and_kill(home.path(), &mut r, 63);
}

#[test]
fn a_supervised_job_killed_by_a_signal_still_reads_as_a_signal() {
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    supervised(&mut r);
    r.start(&job(62, &["sh", "-c", "kill -KILL $$"], 64))
        .unwrap();
    let u = wait_ended(&mut r, STARVED);
    // The supervisor must not turn the signal into an ordinary exit 137.
    assert_eq!(u.exit_code, None, "{u:?}");
    let error = u.error.unwrap_or_default();
    assert!(
        error.contains("SIGKILL") || error.contains("memory cap"),
        "{error}"
    );
}

#[test]
fn a_job_that_ended_while_the_agent_was_down_is_reported_with_its_real_exit() {
    let home = tempfile::tempdir().unwrap();
    let jobs = Paths::new(home.path()).agent_state_dir().join("jobs");
    let (marker, pid_file) = (jobs.join("61.exit"), jobs.join("61.pid"));
    let mut first = runner(home.path(), &["sh"]);
    supervised(&mut first);
    let mut task = job(61, &["sh", "-c", "echo from the job; exit 7"], 64);
    task.attempts = 2;
    first.start(&task).unwrap();
    wait_until("the supervisor to record the exit", STARVED, || {
        marker.exists()
    });
    // The agent dies before it reaps the job: nothing cleans up, the files stay.
    std::mem::forget(first);

    let mut restarted = runner(home.path(), &["sh"]);
    let updates = restarted.poll();
    let [u] = updates.as_slice() else {
        panic!("expected the job's report, got {updates:?}");
    };
    assert_eq!((u.id, u.attempt, u.status), (61, 2, JobStatus::Failed));
    assert_eq!(u.exit_code, Some(7));
    assert!(u
        .output_tail
        .as_deref()
        .unwrap_or("")
        .contains("from the job"));
    // The files stay until the report has gone out; a second crash would repeat it.
    assert!(pid_file.exists() && marker.exists());
    assert!(restarted.poll().is_empty());
    assert!(!pid_file.exists() && !marker.exists());
}

#[test]
fn a_pause_the_job_did_not_honour_is_applied_again_on_the_next_poll() {
    use nix::sys::signal::{killpg, Signal};
    let home = tempfile::tempdir().unwrap();
    let mut r = runner(home.path(), &["sh"]);
    let log = Paths::new(home.path()).job_log_file(64);
    r.start(&job(
        64,
        &["sh", "-c", "while :; do echo tick; sleep 0.05; done"],
        64,
    ))
    .unwrap();
    let root = r.running()[0].pid.unwrap();
    wait_until("the job to tick", STARVED, || {
        std::fs::read_to_string(&log).is_ok_and(|t| t.contains("tick"))
    });
    r.pause(64).unwrap();
    wait_until("the group to stop", STARVED, || group_stopped(root));

    // A SIGSTOP that lands while a job is still executing its wrapper chain can be lost; the
    // same thing happens when something else continues the group behind the runner's back.
    killpg(nix::unistd::Pid::from_raw(root as i32), Signal::SIGCONT).unwrap();
    wait_until("the group to run again", STARVED, || !group_stopped(root));
    r.poll();
    wait_until("the pause to hold again", STARVED, || group_stopped(root));
    assert_eq!(r.running()[0].status, JobStatus::Paused);

    r.kill(64).unwrap();
    wait_ended(&mut r, STARVED);
}
