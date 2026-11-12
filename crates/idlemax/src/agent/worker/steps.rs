//! Adapter: the phases of one worker launch, run on a thread of their own so a slow git,
//! setup or Herdr call never holds the manager: the worktree, the setup command, the baseline
//! check, the agent's start in its pane, the check of its real environment, and the brief.
//! A cancel flag ends it between phases; a kill reaches a running setup or check by its tag.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use idlemax_core::types::{Job, JobId, UnixMs};
use idlemax_core::worker::launch::{
    agent_args, headless_args, mcp_server_names, pane_env, Session,
};
use idlemax_core::worker::{WorkerAction, WorkerCli, WorkerSpec};

use super::cmd::{Cmd, Commands};
use super::gitdir::{self, GitSnapshot};
use super::host::{Handle, WorkerHost};
use super::procs::Procs;
use super::sandbox::{self, GitDirs, SandboxPlan, Wrapped};
use super::worktree::{self, NewWorktree};

mod pane;

pub use pane::brief_line;

const GIT_LIMIT: Duration = Duration::from_secs(10);

/// Everything one launch needs; moved onto its thread.
pub struct LaunchCtx {
    pub host: Arc<dyn WorkerHost>,
    pub commands: Arc<dyn Commands>,
    pub procs: Arc<Mutex<dyn Procs>>,
    pub worktrees_dir: PathBuf,
    /// Where the copy of the repository's git config and hooks is kept during a launch.
    pub snapshots_dir: PathBuf,
    pub job: Job,
    pub worker: WorkerSpec,
    pub tag: u64,
    /// The session to start (a new Claude id) or to continue.
    pub session: Option<String>,
    pub cancel: Arc<AtomicBool>,
    /// The owner paused the worker: the launch starts no agent and sends no brief meanwhile.
    pub paused: Arc<AtomicBool>,
    pub now_ms: UnixMs,
    /// How often the agent's status is read while waiting for `idle`.
    pub poll: Duration,
    /// How long the agent may take to reach `idle` (60 s).
    pub idle_wait: Duration,
    /// The OS sandbox the CLI runs in (A13); None runs it with the soft fences only.
    pub sandbox: Option<SandboxPlan>,
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
    /// `HEAD` of the worktree when this launch began: a commit after it is this launch's.
    pub start_head: Option<String>,
    /// The repository's shared git config and hooks before the worker started.
    pub git: Option<GitSnapshot>,
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
    /// The agent runs but a pause arrived before its brief: the manager freezes it and sends
    /// the brief once the pause is lifted (A24).
    Held(Progress),
}

enum Stop {
    Cancelled,
    Failed(String),
    Held,
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
            Err(Stop::Held) => Outcome::Held(progress),
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
    p.start_head = Some(
        git(ctx, &folder, &["rev-parse", "HEAD"])?
            .trim()
            .to_string(),
    );
    let args = args_for(ctx, &folder)?;
    let sandbox = match &ctx.sandbox {
        Some(plan) => Some(sandbox_for(ctx, plan, &folder)?),
        None => None,
    };
    let remotes = p.remotes.clone();
    hold_while_paused(ctx)?;
    let handle = pane::start_checked(ctx, p, &folder, &remotes, (&args, sandbox.as_ref()))?;
    cancelled(ctx)?;
    if ctx.paused.load(Ordering::SeqCst) {
        return Err(Stop::Held);
    }
    pane::send_brief(ctx, &handle, &folder)
}

/// Waits while the worker is paused, so no agent starts under a pause.
fn hold_while_paused(ctx: &LaunchCtx) -> Result<(), Stop> {
    while ctx.paused.load(Ordering::SeqCst) {
        cancelled(ctx)?;
        std::thread::sleep(ctx.poll);
    }
    Ok(())
}

/// The worktree, the setup and the baseline of a first launch, then the brief file.
fn prepare(ctx: &LaunchCtx, p: &mut Progress) -> Result<PathBuf, Stop> {
    let w = &ctx.worker;
    // Only a folder under the worktrees dir with this Run's marker is reused.
    let reuse = w
        .worktree
        .as_ref()
        .map(PathBuf::from)
        .filter(|f| worktree::ours(&ctx.worktrees_dir, f, run_id(w)).is_some());
    let reused = reuse.is_some();
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
    // A reused folder holds the worker's edits: its setup ran and its baseline describes the
    // base commit, so neither runs again over the edited code (D8).
    if w.action == WorkerAction::Launch && !reused {
        setup_and_baseline(ctx, p, &folder)?;
    }
    // Last before the worker starts: whatever changes after this is the worker's.
    let copy = ctx.snapshots_dir.join(ctx.job.id.to_string());
    let snapshot = gitdir::take(ctx.commands.as_ref(), &folder, &copy).map_err(Stop::Failed)?;
    p.git = Some(snapshot);
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
    let fence = Fence {
        job: ctx.job.id,
        tag: ctx.tag,
        remotes: &p.remotes,
    };
    let code = run_project(ctx.commands.as_ref(), &fence, folder, argv, limit_secs);
    cancelled(ctx)?;
    code.map_err(Stop::Failed)
}

/// What a project command carries so code a worker wrote meets the pane's push block.
pub struct Fence<'a> {
    pub job: JobId,
    pub tag: u64,
    pub remotes: &'a [String],
}

/// A project command in `folder` with the pane's environment pairs, the tag and the throttle
/// (A16): its exit code, or None when it passed its limit.
pub fn run_project(
    commands: &dyn Commands,
    fence: &Fence<'_>,
    folder: &Path,
    argv: &[String],
    limit_secs: u64,
) -> Result<Option<i32>, String> {
    let (program, args) = argv.split_first().ok_or("empty command")?;
    let cmd = Cmd {
        program: program.clone(),
        args: args.to_vec(),
        cwd: Some(folder.to_path_buf()),
        env: pane_env(fence.job, fence.tag, fence.remotes, &[]),
        stdin: None,
        limit: Duration::from_secs(limit_secs),
        throttle: true,
        tag: Some(fence.tag),
    };
    match commands.run(&cmd) {
        Ok(out) => Ok(out.code.or(Some(-1))),
        Err(super::cmd::CmdError::TimedOut) => Ok(None),
        Err(e) => Err(format!("{program} could not run: {e}")),
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
    let session = ctx.session.as_deref().unwrap_or_default();
    let session = match w.action {
        WorkerAction::Resume => Session::Resume(session),
        _ => Session::New(session),
    };
    if ctx.host.is_headless() {
        return headless_args(w.cli, &w.model, session).map_err(|e| Stop::Failed(e.to_string()));
    }
    let common = git(ctx, folder, &["rev-parse", "--git-common-dir"])?;
    let common = folder.join(common.trim()).display().to_string();
    let servers = match w.cli {
        // Fails closed: without the names no server can be switched off.
        WorkerCli::Codex => mcp_server_names(&run_ok(ctx, "codex", &["mcp", "list"], None)?)
            .map_err(|e| Stop::Failed(e.to_string()))?,
        _ => Vec::new(),
    };
    agent_args(w.cli, &w.model, session, &common, &servers).map_err(|e| Stop::Failed(e.to_string()))
}

/// The sandbox of this launch: its git dirs, then the fence, the proxy and the wrap (A13).
fn sandbox_for(ctx: &LaunchCtx, plan: &SandboxPlan, folder: &Path) -> Result<Wrapped, Stop> {
    let dir = |flag: &str| {
        git(ctx, folder, &["rev-parse", "--path-format=absolute", flag])
            .map(|out| PathBuf::from(out.trim()))
    };
    let dirs = GitDirs {
        common: dir("--git-common-dir")?,
        admin: dir("--git-dir")?,
    };
    sandbox::wrap(plan, (ctx.job.id, ctx.tag), ctx.worker.cli, folder, &dirs).map_err(Stop::Failed)
}

fn cancelled(ctx: &LaunchCtx) -> Result<(), Stop> {
    match ctx.cancel.load(Ordering::SeqCst) {
        true => Err(Stop::Cancelled),
        false => Ok(()),
    }
}

fn git(ctx: &LaunchCtx, dir: &Path, args: &[&str]) -> Result<String, Stop> {
    let all = crate::safe_git::args(dir, args);
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
