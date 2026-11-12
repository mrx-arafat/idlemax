//! Tests: the worker manager over the headless host and a stub `claude` script, with the real
//! process table and real git in a temp dir. No test starts `herdr`, `claude` or `codex`.
//! A launch reads the stub's real environment, which macOS hides for its own `/bin/sh`, so the
//! tests that reach the brief run on Linux only; `headless/tests.rs` covers the host there.

use std::os::unix::fs::PermissionsExt;

use idlemax_core::config::WorkerHostChoice;
use idlemax_core::reconcile::KillOrder;

use idlemax_core::types::DeviceProfile;

use super::super::headless::HeadlessHost;
use super::super::host::HostKind;
use super::super::procs::{Member, OsProcs, Procs};
use super::*;
use crate::agent::runner::os::Sig;

/// A rig whose manager starts workers on the headless host; `stub` is the body of `claude`.
pub(super) fn headless_rig(stub: &str) -> Rig {
    let mut rig = rig();
    let bin = rig._temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{stub}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    rig.workers = manager(&rig, &bin);
    rig
}

/// The real process table, whose `environ` answers a little late, so a stub that re-executes
/// itself with a credential has done so by the time its environment is read.
struct LateEnviron(OsProcs);

impl Procs for LateEnviron {
    fn environ(&mut self, pid: u32) -> Option<Vec<(String, String)>> {
        std::thread::sleep(Duration::from_millis(300));
        self.0.environ(pid)
    }
    fn members(&mut self, shell: u32, tag: u64, started_ms: i64) -> Vec<Member> {
        self.0.members(shell, tag, started_ms)
    }
    fn alive(&mut self, members: &[Member], tag: u64, started_ms: i64) -> Vec<u32> {
        self.0.alive(members, tag, started_ms)
    }
    fn rss(&mut self, pids: &[u32]) -> u64 {
        self.0.rss(pids)
    }
    fn signal(&mut self, pids: &[u32], sig: Sig) {
        self.0.signal(pids, sig);
    }
    fn top_memory(&mut self, n: usize, exclude: &[u32]) -> String {
        self.0.top_memory(n, exclude)
    }
}

fn manager(rig: &Rig, bin: &Path) -> Workers {
    let host = HeadlessHost::with_bin_dir(rig.paths.clone(), bin.to_path_buf());
    let procs: Arc<Mutex<dyn Procs>> = Arc::new(Mutex::new(LateEnviron(OsProcs::new().unwrap())));
    let mut workers = Workers::new(
        rig.host.clone(),
        rig.cmds.clone(),
        procs,
        rig.paths.clone(),
        "box".into(),
    )
    .with_headless(Arc::new(host));
    workers.set_host_policy(WorkerHostChoice::Headless, None);
    workers.set_sandbox_policy(
        idlemax_core::worker::sandbox::SandboxSetting::Off,
        Default::default(),
    );
    workers.set_allowlist(vec!["claude".into(), "make".into()]);
    workers.poll = Duration::from_millis(2);
    workers.background = None;
    workers
}

/// Whether a process of the group the stub leads is still there, read with `killpg(pgid, 0)`
/// so it works where there is no `/proc`.
fn group_alive(pgid: u32) -> bool {
    let pgid = nix::unistd::Pid::from_raw(i32::try_from(pgid).unwrap());
    nix::sys::signal::killpg(pgid, None).is_ok()
}

fn stop_order() -> KillOrder {
    KillOrder {
        reason: "stopped by the owner".into(),
        owner_stop: true,
        retryable: false,
    }
}

fn worktree_of(updates: &[JobUpdate]) -> PathBuf {
    let report = updates.iter().rev().find_map(|u| u.worker.as_ref());
    PathBuf::from(report.unwrap().worktree.clone().unwrap())
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "macOS hides the environment of /bin/sh")]
fn a_headless_worker_commits_and_the_launch_ends_succeeded_with_its_output_in_the_job_log() {
    let mut rig = headless_rig(
        "cat >/dev/null\necho working\necho hi > added.txt\ngit add added.txt\n\
         git -c user.name=T -c user.email=t@test.local -c core.hooksPath=/dev/null \
         -c commit.gpgsign=false commit -qm add",
    );
    let mut now = 1_000;
    let job = job(&rig, WorkerAction::Launch);
    rig.workers.handle(Msg::Start(Box::new(job)), now, now);
    let updates = drive(&mut rig, &mut now, terminal);
    assert_eq!(last(&updates).status, JobStatus::Succeeded, "{updates:#?}");
    assert!(briefed(&updates));
    assert!(rig.host.starts.lock().unwrap().is_empty(), "Herdr was used");
    let log = std::fs::read_to_string(rig.paths.job_log_file(7)).unwrap();
    assert!(log.contains("via headless ==="), "{log}");
    assert!(log.contains("working\n"), "{log}");
    assert!(
        log.ends_with("=== idlemax: succeeded (exit 0) ===\n"),
        "{log}"
    );
    assert!(worktree_of(&updates).join("added.txt").exists());
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "macOS hides the environment of /bin/sh")]
fn an_agent_that_exits_with_an_error_fails_the_job_and_keeps_its_last_lines() {
    let mut rig = headless_rig("cat >/dev/null\necho 'usage limit reached'\nexit 1");
    let mut now = 1_000;
    let job = job(&rig, WorkerAction::Launch);
    rig.workers.handle(Msg::Start(Box::new(job)), now, now);
    let updates = drive(&mut rig, &mut now, terminal);
    let end = last(&updates);
    assert_eq!(end.status, JobStatus::Failed);
    assert!(!end.retryable);
    assert_eq!(end.error.as_deref(), Some("the agent exited with code 1"));
    let text = end.worker.as_ref().unwrap().end_text.clone().unwrap();
    assert!(text.contains("usage limit reached"), "{text}");
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "macOS hides the environment of /bin/sh")]
fn a_stop_then_a_resume_continues_the_same_session_with_resume() {
    let mut rig = headless_rig("echo \"$*\" >> .argv\ncat >/dev/null\nsleep 30");
    let mut now = 1_000;
    let first = job(&rig, WorkerAction::Launch);
    let started = rig.workers.handle(Msg::Start(Box::new(first)), now, now);
    let session = started[0].worker.as_ref().unwrap().session.clone().unwrap();
    let mut updates = drive(&mut rig, &mut now, briefed);
    rig.workers.handle(
        Msg::Kill {
            id: 7,
            order: stop_order(),
        },
        now,
        now,
    );
    updates.extend(drive(&mut rig, &mut now, terminal));
    assert_eq!(last(&updates).status, JobStatus::Killed);
    let folder = worktree_of(&updates);
    assert!(folder.join(".argv").exists());

    let mut again = job(&rig, WorkerAction::Resume);
    let worker = again.spec.worker.as_mut().unwrap();
    worker.session = Some(session.clone());
    worker.worktree = Some(folder.display().to_string());
    rig.workers.handle(Msg::Start(Box::new(again)), now, now);
    drive(&mut rig, &mut now, briefed);
    rig.workers.handle(
        Msg::Kill {
            id: 7,
            order: stop_order(),
        },
        now,
        now,
    );
    drive(&mut rig, &mut now, terminal);
    let argv = std::fs::read_to_string(folder.join(".argv")).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    let lean = "-p --model sonnet --strict-mcp-config --permission-mode bypassPermissions";
    assert_eq!(lines.len(), 2, "{argv}");
    assert_eq!(lines[0], format!("{lean} --session-id {session}"));
    assert_eq!(lines[1], format!("{lean} --resume {session}"));
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "macOS hides the environment of /bin/sh")]
fn a_restarted_agent_never_takes_a_headless_worker_back_and_kills_what_is_left() {
    let mut rig = headless_rig("echo $$ > .pid\ncat >/dev/null\nsleep 30");
    let mut now = 1_000;
    let job = job(&rig, WorkerAction::Launch);
    rig.workers.handle(Msg::Start(Box::new(job)), now, now);
    let updates = drive(&mut rig, &mut now, briefed);
    rig.workers.tick(now + 2_000, now + 2_000);
    let folder = worktree_of(&updates);
    let pid: u32 = std::fs::read_to_string(folder.join(".pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(group_alive(pid));

    // The agent restarts: a new manager reads the saved record.
    let bin = rig._temp.path().join("bin");
    let mut fresh = manager(&rig, &bin);
    let lost = fresh.recover(now + 5_000, now + 5_000);
    assert_eq!(lost.len(), 1, "{lost:#?}");
    assert_eq!(lost[0].status, JobStatus::Killed);
    assert!(lost[0].retryable);
    assert!(!fresh.is_tracking());
    let started = Instant::now();
    while group_alive(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a process of group {pid} left"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_host_of_a_start_follows_the_setting_the_profile_and_what_herdr_answers() {
    let mut rig = headless_rig("cat >/dev/null");
    *rig.host.available.lock().unwrap() = Some(false);
    let mut now = 1_000;
    let note = |rig: &mut Rig, now: i64| {
        let job = job(rig, WorkerAction::Launch);
        let first = rig.workers.handle(Msg::Start(Box::new(job)), now, now);
        first[0].error.clone()
    };
    // `auto` with a Herdr that does not answer falls back, and says so in the first update.
    rig.workers.set_host_policy(WorkerHostChoice::Auto, None);
    assert_eq!(
        note(&mut rig, now).as_deref(),
        Some("headless: herdr did not answer")
    );
    rig.workers.handle(
        Msg::Kill {
            id: 7,
            order: stop_order(),
        },
        now,
        now,
    );
    drive(&mut rig, &mut now, terminal);
    // `herdr` is taken as set, and a production device is headless whatever the key says.
    rig.workers.set_host_policy(WorkerHostChoice::Herdr, None);
    assert_eq!(note(&mut rig, now), None);
    assert_eq!(rig.workers.tracked[&7].kind, HostKind::Herdr);
    rig.workers.handle(
        Msg::Kill {
            id: 7,
            order: stop_order(),
        },
        now,
        now,
    );
    drive(&mut rig, &mut now, terminal);
    let production = Some(DeviceProfile::Production);
    rig.workers
        .set_host_policy(WorkerHostChoice::Herdr, production);
    assert_eq!(note(&mut rig, now), None);
    assert_eq!(rig.workers.tracked[&7].kind, HostKind::Headless);
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "macOS hides the environment of /bin/sh")]
fn a_stop_kills_a_stubborn_group_after_the_grace_and_leaves_the_uncommitted_file() {
    let mut rig = headless_rig(
        "echo $$ > .pid\ntrap '' TERM\necho wip > draft.txt\ncat >/dev/null\nsleep 30",
    );
    let mut now = 1_000;
    let job = job(&rig, WorkerAction::Launch);
    rig.workers.handle(Msg::Start(Box::new(job)), now, now);
    let mut updates = drive(&mut rig, &mut now, briefed);
    let folder = worktree_of(&updates);
    let pid: u32 = std::fs::read_to_string(folder.join(".pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    rig.workers.handle(
        Msg::Kill {
            id: 7,
            order: stop_order(),
        },
        now,
        now,
    );
    now += 1_000;
    let early = rig.workers.tick(now, now);
    assert!(early.is_empty(), "ended while the group was alive");
    assert!(group_alive(pid));
    // Still there after the grace: the manager kills it by pid.
    updates.extend(drive(&mut rig, &mut now, terminal));
    assert_eq!(last(&updates).status, JobStatus::Killed);
    let started = Instant::now();
    while group_alive(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a process of group {pid} left"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(folder.join("draft.txt").exists());
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "macOS hides the environment of /bin/sh")]
fn a_credential_in_the_child_environment_blocks_the_brief() {
    let mut rig = headless_rig("exec env GH_TOKEN=secret sh -c 'cat; sleep 30'");
    let mut now = 1_000;
    let job = job(&rig, WorkerAction::Launch);
    rig.workers.handle(Msg::Start(Box::new(job)), now, now);
    let updates = drive(&mut rig, &mut now, terminal);
    let end = last(&updates);
    assert_eq!(end.status, JobStatus::Failed, "{updates:#?}");
    let error = end.error.clone().unwrap();
    assert!(
        error.contains("GH_TOKEN") && error.contains("IDLEMAX_JOB_ID"),
        "{error}"
    );
    let log = std::fs::read_to_string(rig.paths.job_log_file(7)).unwrap();
    assert!(!log.contains(".brief.md"), "the brief was sent: {log}");
}

#[test]
fn herdr_is_asked_at_most_once_in_thirty_seconds() {
    let mut rig = headless_rig("cat >/dev/null");
    rig.workers.set_host_policy(WorkerHostChoice::Auto, None);
    *rig.host.available.lock().unwrap() = Some(false);
    let kind = |rig: &mut Rig, now| rig.workers.choose_host(now).unwrap().0;
    assert_eq!(kind(&mut rig, 1_000), HostKind::Headless);
    *rig.host.available.lock().unwrap() = Some(true);
    assert_eq!(kind(&mut rig, 30_999), HostKind::Headless);
    assert_eq!(kind(&mut rig, 31_000), HostKind::Herdr);
}
