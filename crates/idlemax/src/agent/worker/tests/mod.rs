//! Tests: the worker manager over a temp repository, a `FakeHost`, `FakeProcs` and scripted
//! project commands; real git runs only inside the temp dir, and no test starts `herdr`,
//! `claude` or `codex`.

mod checks;
mod edges;
mod stops;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{CostClass, Job, JobSpec, JobStatus};
use idlemax_core::worker::report::WorkerPhase;
use idlemax_core::worker::{Baseline, WorkerAction, WorkerCli, WorkerSpec};
use idlemax_core::Paths;

use super::cmd::{Cmd, CmdError, Commands, OsCommands, Output};
use super::host::{AgentStatus, FakeHost};
use super::procs::FakeProcs;
use super::{Msg, Workers};

const SHELL: u32 = 100;
const AGENT: u32 = 101;

/// Real git kept off the developer's config; every other program answers from `script`,
/// and a program in `blocking` waits until its tagged pid is killed.
struct TestCmds {
    git: OsCommands,
    calls: Mutex<Vec<Cmd>>,
    script: Mutex<HashMap<String, i32>>,
    blocking: Mutex<Option<(String, Arc<Mutex<FakeProcs>>)>>,
    /// What a Codex reviewer writes to its `-o` file.
    review_answer: Mutex<Option<String>>,
}

impl Commands for TestCmds {
    fn run(&self, cmd: &Cmd) -> Result<Output, CmdError> {
        self.calls.lock().unwrap().push(cmd.clone());
        if cmd.program == "git" {
            let mut cmd = cmd.clone();
            cmd.env
                .push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
            cmd.env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
            return self.git.run(&cmd);
        }
        let blocking = self.blocking.lock().unwrap().clone();
        if let Some((program, procs)) = blocking.filter(|(p, _)| *p == cmd.program) {
            procs.lock().unwrap().tagged.insert(900);
            let started = Instant::now();
            while procs.lock().unwrap().tagged.contains(&900) {
                if started.elapsed() > Duration::from_secs(5) {
                    return Err(CmdError::Io(format!("{program} was never killed")));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            return Ok(out(None));
        }
        if let Some(answer) = self.review_answer.lock().unwrap().clone() {
            let out = cmd.args.iter().position(|a| a == "-o");
            if let Some(path) = out.and_then(|i| cmd.args.get(i + 1)) {
                std::fs::write(path, answer).unwrap();
            }
        }
        let code = self.script.lock().unwrap().get(&cmd.program).copied();
        Ok(out(Some(code.unwrap_or(0))))
    }
}

fn out(code: Option<i32>) -> Output {
    Output {
        code,
        stdout: String::new(),
        stderr: String::new(),
    }
}

struct Rig {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    paths: Paths,
    host: Arc<FakeHost>,
    procs: Arc<Mutex<FakeProcs>>,
    cmds: Arc<TestCmds>,
    workers: Workers,
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(["-C", dir.to_str().unwrap()])
        .args(args)
        .envs([
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rig() -> Rig {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    for kv in [
        ["user.name", "T"],
        ["user.email", "t@test.local"],
        ["commit.gpgsign", "false"],
    ] {
        git(&repo, &["config", kv[0], kv[1]]);
    }
    std::fs::write(repo.join("file.txt"), "one\n").unwrap();
    git(&repo, &["add", "file.txt"]);
    git(&repo, &["commit", "-m", "one"]);
    let paths = Paths::new(temp.path().join("state"));
    let host = Arc::new(FakeHost::default());
    let mut fake = FakeProcs::default();
    fake.live.extend([SHELL, AGENT]);
    fake.environs
        .insert(AGENT, vec![("PATH".into(), "/bin".into())]);
    let procs = Arc::new(Mutex::new(fake));
    let leaving = Arc::clone(&procs);
    *host.on_stop.lock().unwrap() = Some(Box::new(move || {
        let mut p = leaving.lock().unwrap();
        let stubborn = p.stubborn.clone();
        p.live.retain(|pid| stubborn.contains(pid));
    }));
    let cmds = Arc::new(TestCmds {
        git: OsCommands::new(),
        calls: Mutex::new(Vec::new()),
        script: Mutex::new(HashMap::new()),
        blocking: Mutex::new(None),
        review_answer: Mutex::new(None),
    });
    let mut workers = Workers::new(
        host.clone(),
        cmds.clone(),
        procs.clone(),
        paths.clone(),
        "mac".into(),
    );
    workers.set_allowlist(vec!["claude".into(), "make".into()]);
    workers.poll = Duration::from_millis(2);
    workers.background = None;
    Rig {
        _temp: temp,
        repo,
        paths,
        host,
        procs,
        cmds,
        workers,
    }
}

fn job(rig: &Rig, action: WorkerAction) -> Job {
    Job {
        id: 7,
        attempts: 1,
        spec: JobSpec {
            name: "im-app-roadmap-1-3".into(),
            command: vec!["idlemax-worker".into(), "claude".into()],
            cost: CostClass::Ai,
            ai_account: Some("max".into()),
            stdin: Some("# Task: add a line".into()),
            max_runtime_secs: 3600,
            worker: Some(WorkerSpec {
                run: 3,
                action,
                project: "App".into(),
                task: "roadmap:1".into(),
                repo: rig.repo.display().to_string(),
                base_branch: Some("main".into()),
                cli: WorkerCli::Claude,
                model: "sonnet".into(),
                setup: vec!["make".into(), "setup".into()],
                check: vec!["make".into(), "check".into()],
                blocked_secs: 120,
                setup_secs: 900,
                check_secs: 600,
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Ticks until `done` holds for the updates so far, at most 5 s.
fn drive(rig: &mut Rig, now: &mut i64, done: impl Fn(&[JobUpdate]) -> bool) -> Vec<JobUpdate> {
    let mut updates = Vec::new();
    let started = Instant::now();
    while !done(&updates) {
        assert!(started.elapsed() < Duration::from_secs(5), "{updates:#?}");
        *now += 2_000;
        updates.extend(rig.workers.tick(*now, *now));
        std::thread::sleep(Duration::from_millis(5));
    }
    updates
}

fn terminal(updates: &[JobUpdate]) -> bool {
    updates.iter().any(|u| u.status.is_terminal())
}

fn briefed(updates: &[JobUpdate]) -> bool {
    updates.iter().any(|u| {
        u.worker
            .as_ref()
            .is_some_and(|w| w.phase == WorkerPhase::Running)
    })
}

fn last(updates: &[JobUpdate]) -> &JobUpdate {
    updates.last().unwrap()
}

#[test]
fn a_launch_is_briefed_after_the_env_check_and_succeeds_when_the_turn_ends() {
    let mut rig = rig();
    for status in [
        AgentStatus::Idle,
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Done,
    ] {
        rig.host.push_state(status, None, &[SHELL, AGENT]);
    }
    let mut now = 1_000;
    let first = rig.workers.handle(
        Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
        now,
        now,
    );
    let session = first[0].worker.as_ref().unwrap().session.clone().unwrap();
    assert_eq!(session.len(), 36, "{session}");
    let updates = drive(&mut rig, &mut now, terminal);
    assert!(briefed(&updates));
    assert_eq!(last(&updates).status, JobStatus::Succeeded);

    let start = &rig.host.starts.lock().unwrap()[0];
    assert!(start.args.contains(&session));
    let folder = start.folder.clone();
    assert!(folder.join("file.txt").exists());
    let marker = super::worktree::marker_of(&folder).unwrap();
    assert_eq!(marker.baseline, Some(0));
    let sent = rig.host.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1.contains(".brief.md"), "{}", sent[0].1);
}

#[test]
fn a_credential_in_the_env_restarts_the_pane_once_and_blocks_the_brief_if_it_stays() {
    for (stays, want) in [(false, JobStatus::Succeeded), (true, JobStatus::Failed)] {
        let mut rig = rig();
        let leaky = vec![("GH_TOKEN".to_string(), "secret".to_string())];
        rig.procs
            .lock()
            .unwrap()
            .environs
            .insert(AGENT, leaky.clone());
        let clean = Arc::clone(&rig.procs);
        let hook = rig.host.on_stop.lock().unwrap().take().unwrap();
        *rig.host.on_stop.lock().unwrap() = Some(Box::new(move || {
            if !stays {
                clean.lock().unwrap().environs.insert(AGENT, Vec::new());
                clean
                    .lock()
                    .unwrap()
                    .environs
                    .get_mut(&AGENT)
                    .unwrap()
                    .push(("A".into(), "b".into()));
                return;
            }
            hook();
        }));
        // The first start's check, the second start's check, then the brief.
        for status in [
            AgentStatus::Idle,
            AgentStatus::Idle,
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Done,
        ] {
            rig.host.push_state(status, None, &[SHELL, AGENT]);
        }
        let mut now = 1_000;
        rig.workers.handle(
            Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
            now,
            now,
        );
        let updates = drive(&mut rig, &mut now, terminal);
        assert_eq!(last(&updates).status, want);
        let starts = rig.host.starts.lock().unwrap().clone();
        assert_eq!(starts.len(), 2);
        assert!(starts[1].env.contains(&("GH_TOKEN".into(), String::new())));
        if stays {
            assert!(rig.host.sent.lock().unwrap().is_empty());
            let error = last(&updates).error.clone().unwrap();
            assert!(
                error.contains("GH_TOKEN") && error.contains("IDLEMAX_JOB_ID"),
                "{error}"
            );
        }
    }
}

#[test]
fn a_program_outside_the_allowlist_fails_the_job_without_a_pane() {
    for program in ["claude", "make"] {
        let mut rig = rig();
        rig.workers.set_allowlist(vec![if program == "claude" {
            "make"
        } else {
            "claude"
        }
        .into()]);
        let updates =
            rig.workers
                .handle(Msg::Start(Box::new(job(&rig, WorkerAction::Launch))), 1, 1);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].status, JobStatus::Failed);
        assert!(!updates[0].retryable);
        assert!(updates[0].error.as_ref().unwrap().contains(program));
        assert!(rig.host.starts.lock().unwrap().is_empty());
    }
}
