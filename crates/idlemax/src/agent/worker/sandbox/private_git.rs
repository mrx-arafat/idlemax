//! Adapter: the private git dir of a sandboxed worker (review findings 1 and 2). The worker's
//! git never writes the owner's repository: inside the sandbox `GIT_DIR` is a folder of
//! idlemax's own, with its own refs, index and objects, and the owner's objects as a read-only
//! alternate. Its `config`, `hooks` and `objects/info` are bound read-only, so its settings stay
//! idlemax's. When the worker's turn ends, idlemax fetches only the Run's own branch from it
//! into the owner's repository, so that branch is the only ref a worker can move.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The private git dir of Run `run` under the state dir.
pub fn dir_of(state: &Path, run: i64) -> PathBuf {
    state.join("sandbox").join("git").join(run.to_string())
}

/// What the sandbox binds of the private git dir: writable as a whole, these parts read-only.
pub fn read_only(dir: &Path) -> Vec<PathBuf> {
    ["config", "hooks", "objects/info"]
        .iter()
        .map(|p| dir.join(p))
        .collect()
}

/// Makes the private git dir of a Run once: the Run's branch at `start`, `HEAD` on it, the
/// worktree's index, and the owner's objects as an alternate. A resumed Run keeps its own.
pub fn prepare(
    dir: &Path,
    common: &Path,
    admin: &Path,
    branch: &str,
    start: &str,
) -> Result<(), String> {
    if dir.join("HEAD").is_file() {
        return Ok(());
    }
    let parent = dir.parent().ok_or("the private git dir has no parent")?;
    crate::state::create_private_dir(parent).map_err(|e| e.to_string())?;
    run(None, &["init", "-q", "--bare", &show(dir)])?;
    let git_dir = Some(dir);
    for (key, value) in [
        ("core.bare", "false"),
        ("gc.auto", "0"),
        ("core.fsmonitor", "false"),
    ] {
        run(git_dir, &["config", key, value])?;
    }
    let alternates = dir.join("objects/info/alternates");
    std::fs::write(
        &alternates,
        format!("{}\n", common.join("objects").display()),
    )
    .map_err(|e| e.to_string())?;
    let head = format!("refs/heads/{branch}");
    run(git_dir, &["update-ref", &head, start])?;
    run(git_dir, &["symbolic-ref", "HEAD", &head])?;
    if admin.join("index").is_file() {
        std::fs::copy(admin.join("index"), dir.join("index")).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Brings the worker's commits on `branch` into the owner's repository: a fetch from the
/// private git dir (which checks every object) into a scratch ref, then the branch moved to
/// it, and the worktree's index read from the new tip. Nothing when the Run has no private dir.
pub fn import(state: &Path, run_id: i64, folder: &Path, branch: &str) -> Result<(), String> {
    let dir = dir_of(state, run_id);
    if !dir.join("HEAD").is_file() {
        return Ok(());
    }
    let head = format!("refs/heads/{branch}");
    let tip = output(
        Some(&dir),
        &["rev-parse", "--verify", "-q", &format!("{head}^{{commit}}")],
    )?;
    let owner = |args: &[&str]| run_in(folder, args);
    let scratch = format!("refs/idlemax/incoming/{run_id}");
    owner(&[
        "fetch",
        "-q",
        "--no-tags",
        "--no-write-fetch-head",
        &show(&dir),
        &format!("+{head}:{scratch}"),
    ])?;
    owner(&["update-ref", &head, tip.trim()])?;
    owner(&["update-ref", "-d", &scratch])?;
    owner(&["read-tree", &head])
}

fn run(git_dir: Option<&Path>, args: &[&str]) -> Result<(), String> {
    output(git_dir, args).map(drop)
}

fn run_in(folder: &Path, args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.args(crate::safe_git::args(folder, args));
    finish(cmd, args)
}

fn output(git_dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(crate::safe_git::SAFE);
    if let Some(dir) = git_dir {
        cmd.arg(format!("--git-dir={}", dir.display()));
    }
    cmd.args(args);
    let out = cmd
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn finish(mut cmd: Command, args: &[&str]) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn show(p: &Path) -> String {
    p.display().to_string()
}
