//! Adapter: the phases of one worker launch, run on a thread of their own so a slow git,
//! setup or Herdr call never holds the manager: the worktree, the setup command, the baseline
//! check, the agent's start in its pane, the check of its real environment, and the brief.
//! A cancel flag ends it between phases; a kill reaches a running setup or check by its tag.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use idlemax_core::types::{Job, UnixMs};
use idlemax_core::worker::brief::{resume_line, start_line};
use idlemax_core::worker::launch::{agent_args, env_problems, mcp_server_names, pane_env, Session};
use idlemax_core::worker::{name, WorkerAction, WorkerCli, WorkerSpec};

use super::cmd::{Cmd, Commands};
use super::host::{AgentStatus, Handle, HostState, Launch, WorkerHost};
use super::procs::Procs;
use super::worktree::{self, NewWorktree};

const GIT_LIMIT: Duration = Duration::from_secs(10);
/// How long a fresh agent may take to reach `idle` before its environment is checked.
const IDLE_WAIT: Duration = Duration::from_secs(60);

/// Everything one launch needs; moved onto its thread.
pub struct LaunchCtx {
    pub host: Arc<dyn WorkerHost>,
    pub commands: Arc<dyn Commands>,
    pub procs: Arc<Mutex<dyn Procs>>,
    pub worktrees_dir: PathBuf,
    pub job: Job,
    pub worker: WorkerSpec,
    pub tag: u64,
    /// The session to start (a new Claude id) or to continue.
    pub session: Option<String>,
    pub cancel: Arc<AtomicBool>,
    pub now_ms: UnixMs,
    /// How often the agent's status is read while waiting for `idle`.
    pub poll: Duration,
}

/// What the launch got done, also when it stopped early.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub folder: Option<PathBuf>,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
    pub handle: Option<Handle>,
    pub session: Option<String>,
    pub shell_pid: Option<u32>,
    /// The repository's remotes, each given a dead push address.
    pub remotes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The agent got its brief and is working.
    Briefed(Progress),
    /// The launch cannot succeed as it is; the job ends final.
    Failed {
        error: String,
        progress: Progress,
    },
    Cancelled(Progress),
}

enum Stop {
    Cancelled,
    Failed(String),
}

pub fn spawn(ctx: LaunchCtx) -> Receiver<Outcome> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut progress = Progress {
            session: ctx.session.clone(),
            ..Default::default()
        };
        let outcome = match launch(&ctx, &mut progress) {
            Ok(()) => Outcome::Briefed(progress),
            Err(Stop::Cancelled) => Outcome::Cancelled(progress),
            Err(Stop::Failed(error)) => Outcome::Failed { error, progress },
        };
        let _ = tx.send(outcome);
    });
    rx
}

fn launch(ctx: &LaunchCtx, p: &mut Progress) -> Result<(), Stop> {
    let repo = Path::new(&ctx.worker.repo);
    p.remotes = git(ctx, repo, &["remote"])?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let folder = prepare(ctx, p)?;
    let args = args_for(ctx, &folder)?;
    let remotes = p.remotes.clone();
    let handle = start_checked(ctx, p, &folder, &remotes, &args)?;
    cancelled(ctx)?;
    let brief = brief_path(&folder).display().to_string();
    let line = match ctx.worker.action {
        WorkerAction::Resume => resume_line(&brief),
        _ => start_line(&brief),
    };
    ctx.host
        .send(&handle, &line)
        .map_err(|e| Stop::Failed(format!("the brief could not be sent: {e}")))
}

/// The worktree, the setup and the baseline of a first launch, then the brief file.
fn prepare(ctx: &LaunchCtx, p: &mut Progress) -> Result<PathBuf, Stop> {
    let w = &ctx.worker;
    let reuse = w.worktree.as_ref().map(PathBuf::from).filter(|f| {
        worktree::marker_of(f)
            .is_some_and(|m| m.run == run_id(w) || w.action == WorkerAction::Resume)
    });
    let folder = match reuse {
        Some(folder) => {
            let marker = worktree::marker_of(&folder);
            p.branch = marker.as_ref().map(|m| m.branch.clone());
            p.base_commit = marker.map(|m| m.base_commit);
            folder
        }
        None if w.action == WorkerAction::Resume => {
            return Err(Stop::Failed("the worktree to resume in is gone".into()))
        }
        None => create(ctx, p)?,
    };
    p.folder = Some(folder.clone());
    cancelled(ctx)?;
    if w.action == WorkerAction::Launch {
        setup_and_baseline(ctx, p, &folder)?;
    }
    write_brief(ctx, &folder)?;
    Ok(folder)
}

fn create(ctx: &LaunchCtx, p: &mut Progress) -> Result<PathBuf, Stop> {
    let w = &ctx.worker;
    let new = NewWorktree {
        repo: Path::new(&w.repo),
        project: &w.project,
        task_id: &w.task,
        run_id: run_id(w),
        base_branch: w.base_branch.as_deref().unwrap_or("HEAD"),
    };
    let info =
        worktree::create_worktree(ctx.commands.as_ref(), &ctx.worktrees_dir, &new, ctx.now_ms)
            .map_err(|e| Stop::Failed(format!("the worktree could not be made: {e}")))?;
    p.branch = Some(info.branch);
    p.base_commit = Some(info.base_commit);
    Ok(info.folder)
}

/// Runs the setup argv, then records the baseline: the dispatcher's result when it names this
/// base commit (A11), else the check argv's exit code, None when there is none or it timed out.
fn setup_and_baseline(ctx: &LaunchCtx, p: &Progress, folder: &Path) -> Result<(), Stop> {
    let w = &ctx.worker;
    if !w.setup.is_empty() {
        match project_cmd(ctx, p, folder, &w.setup, w.setup_secs)? {
            Some(0) => {}
            Some(code) => return Err(Stop::Failed(format!("the setup command exited {code}"))),
            None => return Err(Stop::Failed("the setup command did not finish".into())),
        }
    }
    cancelled(ctx)?;
    let known = w
        .baseline
        .as_ref()
        .filter(|b| Some(&b.base_commit) == p.base_commit.as_ref());
    let baseline = match known {
        Some(b) => b.passed.map(|ok| if ok { 0 } else { 1 }),
        None if w.check.is_empty() => None,
        None => project_cmd(ctx, p, folder, &w.check, w.check_secs)?,
    };
    worktree::record_baseline(folder, baseline)
        .map_err(|e| Stop::Failed(format!("the baseline could not be recorded: {e}")))
}

/// A project command with the pane's environment pairs, the tag and the throttle (A16); its
/// exit code, or None when it passed its limit.
fn project_cmd(
    ctx: &LaunchCtx,
    p: &Progress,
    folder: &Path,
    argv: &[String],
    limit_secs: u64,
) -> Result<Option<i32>, Stop> {
    let (program, args) = argv
        .split_first()
        .ok_or(Stop::Failed("empty command".into()))?;
    let cmd = Cmd {
        program: program.clone(),
        args: args.to_vec(),
        cwd: Some(folder.to_path_buf()),
        env: pane_env(ctx.job.id, ctx.tag, &p.remotes, &[]),
        stdin: None,
        limit: Duration::from_secs(limit_secs),
        throttle: true,
        tag: Some(ctx.tag),
    };
    let out = ctx.commands.run(&cmd);
    cancelled(ctx)?;
    match out {
        Ok(out) => Ok(out.code.or(Some(-1))),
        Err(super::cmd::CmdError::TimedOut) => Ok(None),
        Err(e) => Err(Stop::Failed(format!("{program} could not run: {e}"))),
    }
}

fn write_brief(ctx: &LaunchCtx, folder: &Path) -> Result<(), Stop> {
    let path = brief_path(folder);
    match ctx.job.spec.stdin.as_deref() {
        Some(text) => {
            use std::io::Write;
            let write = crate::state::private_file()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()));
            write.map_err(|e| Stop::Failed(format!("{}: {e}", path.display())))
        }
        None if path.exists() => Ok(()),
        None => Err(Stop::Failed("the job carries no brief".into())),
    }
}

/// `<folder>.brief.md`, beside the worktree and outside the repository.
pub fn brief_path(folder: &Path) -> PathBuf {
    PathBuf::from(format!("{}.brief.md", folder.display()))
}

fn args_for(ctx: &LaunchCtx, folder: &Path) -> Result<Vec<String>, Stop> {
    let w = &ctx.worker;
    let common = git(ctx, folder, &["rev-parse", "--git-common-dir"])?;
    let common = folder.join(common.trim()).display().to_string();
    let servers = match w.cli {
        // Fails closed: without the names no server can be switched off.
        WorkerCli::Codex => mcp_server_names(&run_ok(ctx, "codex", &["mcp", "list"], None)?)
            .map_err(|e| Stop::Failed(e.to_string()))?,
        _ => Vec::new(),
    };
    let session = ctx.session.as_deref().unwrap_or_default();
    let session = match w.action {
        WorkerAction::Resume => Session::Resume(session),
        _ => Session::New(session),
    };
    agent_args(w.cli, &w.model, session, &common, &servers).map_err(|e| Stop::Failed(e.to_string()))
}

/// Starts the agent and checks its real environment; a pane whose environment holds a
/// credential is made once more with those names blanked, and never briefed if they stay.
fn start_checked(
    ctx: &LaunchCtx,
    p: &mut Progress,
    folder: &Path,
    remotes: &[String],
    args: &[String],
) -> Result<Handle, Stop> {
    let mut blank = Vec::new();
    for attempt in 0..2 {
        let launch = Launch {
            name: ctx.job.spec.name.clone(),
            tab: name::tab_label(&ctx.worker.project),
            folder: folder.to_path_buf(),
            env: pane_env(ctx.job.id, ctx.tag, remotes, &blank),
            cli: ctx.worker.cli,
            args: args.to_vec(),
        };
        let handle = ctx
            .host
            .start(&launch)
            .map_err(|e| Stop::Failed(format!("the worker could not be started: {e}")))?;
        p.handle = Some(handle.clone());
        let state = wait_idle(ctx, &handle)?;
        p.shell_pid = state.shell_pid;
        p.session = state.session.clone().or(p.session.take());
        let pid = state.foreground.first().copied().or(state.shell_pid);
        let vars = pid.and_then(|pid| ctx.procs.lock().ok()?.environ(pid));
        let problems = match vars {
            Some(vars) => env_problems(&vars),
            None => {
                return Err(close(
                    ctx,
                    p,
                    "the worker's environment could not be read".into(),
                ))
            }
        };
        if problems.is_empty() {
            return Ok(handle);
        }
        let _ = ctx.host.stop(&handle);
        p.handle = None;
        if attempt == 1 {
            return Err(Stop::Failed(format!(
                "the worker's environment still holds {}; guard each export of it in your shell \
                 start-up file with [ -z \"$IDLEMAX_JOB_ID\" ]",
                problems.join(", ")
            )));
        }
        blank = problems;
    }
    Err(Stop::Failed("the worker could not be started".into()))
}

fn wait_idle(ctx: &LaunchCtx, handle: &Handle) -> Result<HostState, Stop> {
    let since = Instant::now();
    loop {
        if ctx.cancel.load(Ordering::SeqCst) {
            return Err(Stop::Cancelled);
        }
        let state = ctx.host.state(handle).ok();
        match state.as_ref().map(|s| s.status) {
            Some(AgentStatus::Idle) => return state.ok_or(Stop::Cancelled),
            Some(AgentStatus::Gone) => {
                return Err(Stop::Failed("the agent exited before it finished".into()))
            }
            _ if since.elapsed() >= IDLE_WAIT => {
                return Err(Stop::Failed(
                    "the worker did not reach idle within 60 s".into(),
                ))
            }
            _ => std::thread::sleep(ctx.poll),
        }
    }
}

/// Closes the pane of a launch that failed and gives the failure.
fn close(ctx: &LaunchCtx, p: &mut Progress, error: String) -> Stop {
    if let Some(handle) = p.handle.take() {
        let _ = ctx.host.stop(&handle);
    }
    Stop::Failed(error)
}

fn cancelled(ctx: &LaunchCtx) -> Result<(), Stop> {
    match ctx.cancel.load(Ordering::SeqCst) {
        true => Err(Stop::Cancelled),
        false => Ok(()),
    }
}

fn git(ctx: &LaunchCtx, dir: &Path, args: &[&str]) -> Result<String, Stop> {
    let mut all = vec!["-C".to_string(), dir.display().to_string()];
    all.extend(args.iter().map(|s| s.to_string()));
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    run_ok(ctx, "git", &all, None)
}

fn run_ok(
    ctx: &LaunchCtx,
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<String, Stop> {
    let cmd = Cmd {
        program: program.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: cwd.map(Path::to_path_buf),
        env: Vec::new(),
        stdin: None,
        limit: GIT_LIMIT,
        throttle: false,
        tag: None,
    };
    let call = format!(
        "{program} {}",
        args.iter().take(3).copied().collect::<Vec<_>>().join(" ")
    );
    match ctx.commands.run(&cmd) {
        Ok(out) if out.code == Some(0) => Ok(out.stdout),
        Ok(out) => Err(Stop::Failed(format!(
            "{call} failed: {}",
            out.stderr.trim()
        ))),
        Err(e) => Err(Stop::Failed(format!("{call}: {e}"))),
    }
}

fn run_id(w: &WorkerSpec) -> u64 {
    u64::try_from(w.run).unwrap_or(0)
}
