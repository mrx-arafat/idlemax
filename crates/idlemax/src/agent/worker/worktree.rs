//! Adapter: git worktree operations for worker tasks.

use std::path::{Path, PathBuf};
use std::time::Duration;

use idlemax_core::types::UnixMs;
use idlemax_core::worker::name;
use serde::{Deserialize, Serialize};

use super::cmd::{Cmd, CmdError, Commands, Output};

const GIT_ADD_LIMIT: Duration = Duration::from_secs(60);
const GIT_CMD_LIMIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub folder: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeMarker {
    pub run: u64,
    pub repo: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub creation_time: UnixMs,
    /// Exit code of the project's check on the base commit; None when not recorded (D8).
    #[serde(default)]
    pub baseline: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("folder {0} is outside worktrees dir {1}")]
    OutsideWorktreesDir(PathBuf, PathBuf),
    #[error("missing marker file {0}")]
    MissingMarker(PathBuf),
    #[error("invalid marker file {0}: {1}")]
    InvalidMarker(PathBuf, String),
    #[error("invalid branch name {0}: must start with idlemax/")]
    InvalidBranchName(String),
    #[error("branch {0} does not match marker branch {1}")]
    BranchMismatch(String, String),
    #[error("command error: {0}")]
    Cmd(#[from] CmdError),
    #[error("git error: {0}")]
    Git(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

fn marker_path_for(folder: &Path) -> PathBuf {
    PathBuf::from(format!("{}.json", folder.display()))
}

/// The marker idlemax wrote beside `folder`, when there is one.
pub fn marker_of(folder: &Path) -> Option<WorktreeMarker> {
    read_marker(&marker_path_for(folder)).ok()
}

/// Records the baseline exit code in the folder's marker.
pub fn record_baseline(folder: &Path, baseline: Option<i32>) -> Result<(), WorktreeError> {
    let path = marker_path_for(folder);
    let mut marker = read_marker(&path)?;
    marker.baseline = baseline;
    write_marker(&path, &marker)
}

fn read_marker(marker_path: &Path) -> Result<WorktreeMarker, WorktreeError> {
    let content = std::fs::read_to_string(marker_path)?;
    serde_json::from_str(&content)
        .map_err(|e| WorktreeError::InvalidMarker(marker_path.to_path_buf(), e.to_string()))
}

fn write_marker(marker_path: &Path, marker: &WorktreeMarker) -> Result<(), WorktreeError> {
    let json = serde_json::to_string_pretty(marker)
        .map_err(|e| WorktreeError::InvalidMarker(marker_path.to_path_buf(), e.to_string()))?;

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(marker_path)?;
        file.write_all(json.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(marker_path, json.as_bytes())?;
    }
    Ok(())
}

/// The owner's global git config stays in force (git-lfs filters, safe.directory), except
/// that no fsmonitor or hook runs; `OsCommands` already sets GIT_TERMINAL_PROMPT=0.
fn git_cmd(dir: &Path, args: &[&str], limit: Duration) -> Cmd {
    Cmd::git(dir, args, limit)
}

fn check_git(out: Output) -> Result<String, WorktreeError> {
    if out.code != Some(0) {
        let msg = if !out.stderr.is_empty() {
            out.stderr
        } else {
            out.stdout
        };
        return Err(WorktreeError::Git(msg.trim().to_string()));
    }
    Ok(out.stdout)
}

/// The marker of `folder` when it is a worktree idlemax made for Run `run`: under the
/// worktrees dir by canonical path, with a marker that names that Run (red team case 9). A
/// marker beside any other folder, such as the owner's checkout, counts for nothing.
pub fn ours(worktrees_dir: &Path, folder: &Path, run: u64) -> Option<WorktreeMarker> {
    validate_worktree_folder(worktrees_dir, folder)
        .ok()
        .filter(|m| m.run == run)
}

fn validate_worktree_folder(
    worktrees_dir: &Path,
    folder: &Path,
) -> Result<WorktreeMarker, WorktreeError> {
    if folder
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(WorktreeError::OutsideWorktreesDir(
            folder.to_path_buf(),
            worktrees_dir.to_path_buf(),
        ));
    }

    let canonical_dir = worktrees_dir.canonicalize().map_err(WorktreeError::Io)?;
    let canonical_folder = if folder.exists() {
        folder.canonicalize().map_err(WorktreeError::Io)?
    } else if let Some(parent) = folder.parent() {
        if parent.exists() {
            parent
                .canonicalize()
                .map_err(WorktreeError::Io)?
                .join(folder.file_name().unwrap_or_default())
        } else {
            return Err(WorktreeError::OutsideWorktreesDir(
                folder.to_path_buf(),
                worktrees_dir.to_path_buf(),
            ));
        }
    } else {
        return Err(WorktreeError::OutsideWorktreesDir(
            folder.to_path_buf(),
            worktrees_dir.to_path_buf(),
        ));
    };

    if !canonical_folder.starts_with(&canonical_dir) || canonical_folder == canonical_dir {
        return Err(WorktreeError::OutsideWorktreesDir(
            folder.to_path_buf(),
            worktrees_dir.to_path_buf(),
        ));
    }

    let marker_path = marker_path_for(folder);
    if !marker_path.exists() {
        return Err(WorktreeError::MissingMarker(marker_path));
    }
    read_marker(&marker_path)
}

fn branch_exists_in_repo(
    commands: &dyn Commands,
    repo: &Path,
    branch: &str,
) -> Result<bool, WorktreeError> {
    let ref_name = format!("refs/heads/{branch}");
    let cmd = git_cmd(repo, &["rev-parse", "--verify", &ref_name], GIT_CMD_LIMIT);
    let out = commands.run(&cmd)?;
    Ok(out.code == Some(0))
}

/// Undoes a `worktree add` whose follow-up failed. The folder was made by that call a moment
/// ago and holds only tracked files, so git's safe remove (no force) suffices; the branch is
/// the one the call created.
fn rollback_worktree(commands: &dyn Commands, repo: &Path, folder: &str, branch: &str) {
    let _ = commands.run(&git_cmd(
        repo,
        &["worktree", "remove", folder],
        GIT_CMD_LIMIT,
    ));
    let _ = commands.run(&git_cmd(repo, &["branch", "-D", branch], GIT_CMD_LIMIT));
}

/// What to create a worktree for.
#[derive(Debug, Clone, Copy)]
pub struct NewWorktree<'a> {
    pub repo: &'a Path,
    pub project: &'a str,
    pub task_id: &'a str,
    pub run_id: u64,
    pub base_branch: &'a str,
}

/// The branch and folder of a task; `-r<run id>` on both when either already exists, so a
/// task tried again never reuses an old branch.
fn pick_branch_and_folder(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    new: &NewWorktree<'_>,
) -> Result<(String, PathBuf), WorktreeError> {
    let p_slug = name::slug(new.project);
    let t_slug = name::task_slug(new.task_id);
    let branch = name::branch_name(&t_slug, None);
    let folder = name::worktree_dir(worktrees_dir, &p_slug, &t_slug, None);
    if folder.exists() || branch_exists_in_repo(commands, new.repo, &branch)? {
        let run = Some(new.run_id);
        return Ok((
            name::branch_name(&t_slug, run),
            name::worktree_dir(worktrees_dir, &p_slug, &t_slug, run),
        ));
    }
    Ok((branch, folder))
}

/// Creates the task's worktree on a new `idlemax/` branch at the base branch, with plain git
/// and no fetch, and writes the marker that proves idlemax made it.
pub fn create_worktree(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    new: &NewWorktree<'_>,
    now_ms: UnixMs,
) -> Result<WorktreeInfo, WorktreeError> {
    let (branch, folder) = pick_branch_and_folder(commands, worktrees_dir, new)?;
    if let Some(parent) = folder.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let folder_str = folder.display().to_string();
    let add = [
        "worktree",
        "add",
        "-b",
        &branch,
        &folder_str,
        new.base_branch,
    ];
    check_git(commands.run(&git_cmd(new.repo, &add, GIT_ADD_LIMIT))?)?;
    let marked = base_commit(commands, &folder).and_then(|base_commit| {
        let marker = WorktreeMarker {
            run: new.run_id,
            repo: new.repo.to_path_buf(),
            branch: branch.clone(),
            base_commit,
            creation_time: now_ms,
            baseline: None,
        };
        write_marker(&marker_path_for(&folder), &marker).map(|()| marker.base_commit)
    });
    match marked {
        Ok(base_commit) => Ok(WorktreeInfo {
            folder,
            branch,
            base_commit,
        }),
        Err(e) => {
            rollback_worktree(commands, new.repo, &folder_str, &branch);
            Err(e)
        }
    }
}

fn base_commit(commands: &dyn Commands, folder: &Path) -> Result<String, WorktreeError> {
    let rev = git_cmd(folder, &["rev-parse", "HEAD"], GIT_CMD_LIMIT);
    Ok(check_git(commands.run(&rev)?)?.trim().to_string())
}

pub fn remove_worktree(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    repo: &Path,
    folder: &Path,
) -> Result<(), WorktreeError> {
    let _marker = validate_worktree_folder(worktrees_dir, folder)?;
    let folder_str = folder.display().to_string();
    let rm_cmd = git_cmd(repo, &["worktree", "remove", &folder_str], GIT_CMD_LIMIT);
    check_git(commands.run(&rm_cmd)?)?;
    Ok(())
}

pub fn delete_branch(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    repo: &Path,
    folder: &Path,
    branch: &str,
) -> Result<(), WorktreeError> {
    let marker = validate_worktree_folder(worktrees_dir, folder)?;

    if !branch.starts_with("idlemax/") {
        return Err(WorktreeError::InvalidBranchName(branch.to_string()));
    }
    if branch != marker.branch {
        return Err(WorktreeError::BranchMismatch(
            branch.to_string(),
            marker.branch,
        ));
    }

    let del_cmd = git_cmd(repo, &["branch", "-D", branch], GIT_CMD_LIMIT);
    check_git(commands.run(&del_cmd)?)?;
    let _ = std::fs::remove_file(marker_path_for(folder));
    Ok(())
}

#[cfg(test)]
mod tests;
