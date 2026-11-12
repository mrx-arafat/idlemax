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

fn git_cmd(dir: &Path, args: &[&str], limit: Duration) -> Cmd {
    let mut full = vec!["-C".to_string(), dir.display().to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    Cmd {
        program: "git".into(),
        args: full,
        cwd: None,
        env: vec![
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ],
        stdin: None,
        limit,
        throttle: false,
        tag: None,
    }
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

fn rollback_worktree(commands: &dyn Commands, repo: &Path, folder_str: &str, branch: &str) {
    let _ = commands.run(&git_cmd(
        repo,
        &["worktree", "remove", "--force", folder_str],
        GIT_CMD_LIMIT,
    ));
    let _ = commands.run(&git_cmd(repo, &["branch", "-D", branch], GIT_CMD_LIMIT));
}

#[allow(clippy::too_many_arguments)]
pub fn create_worktree(
    commands: &dyn Commands,
    worktrees_dir: &Path,
    repo: &Path,
    project: &str,
    task_id: &str,
    run_id: u64,
    base_branch: &str,
    now_ms: UnixMs,
) -> Result<WorktreeInfo, WorktreeError> {
    let p_slug = name::slug(project);
    let t_slug = name::task_slug(task_id);

    let base_branch_name = name::branch_name(&t_slug, None);
    let base_folder = name::worktree_dir(worktrees_dir, &p_slug, &t_slug, None);

    let exists = base_folder.exists() || branch_exists_in_repo(commands, repo, &base_branch_name)?;

    let (branch, folder) = if exists {
        (
            name::branch_name(&t_slug, Some(run_id)),
            name::worktree_dir(worktrees_dir, &p_slug, &t_slug, Some(run_id)),
        )
    } else {
        (base_branch_name, base_folder)
    };

    if let Some(parent) = folder.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let folder_str = folder.display().to_string();
    let add_cmd = git_cmd(
        repo,
        &["worktree", "add", "-b", &branch, &folder_str, base_branch],
        GIT_ADD_LIMIT,
    );
    check_git(commands.run(&add_cmd)?)?;

    let rev_cmd = git_cmd(&folder, &["rev-parse", "HEAD"], GIT_CMD_LIMIT);
    let base_commit = match check_git(commands.run(&rev_cmd)?) {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            rollback_worktree(commands, repo, &folder_str, &branch);
            return Err(e);
        }
    };

    let marker = WorktreeMarker {
        run: run_id,
        repo: repo.to_path_buf(),
        branch: branch.clone(),
        base_commit: base_commit.clone(),
        creation_time: now_ms,
    };
    if let Err(e) = write_marker(&marker_path_for(&folder), &marker) {
        rollback_worktree(commands, repo, &folder_str, &branch);
        return Err(e);
    }

    Ok(WorktreeInfo {
        folder,
        branch,
        base_commit,
    })
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
mod tests {
    use super::*;
    use crate::agent::worker::cmd::OsCommands;

    fn setup_temp_repo() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
        let temp = tempfile::tempdir().unwrap();
        let (repo, worktrees) = (temp.path().join("repo"), temp.path().join("worktrees"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&worktrees).unwrap();

        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-C", repo.to_str().unwrap()])
                .args(args)
                .envs([
                    ("GIT_CONFIG_GLOBAL", "/dev/null"),
                    ("GIT_CONFIG_NOSYSTEM", "1"),
                ])
                .output()
                .unwrap()
        };

        run(&["init", "-b", "main"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "user.email", "test@test.local"]);
        run(&["config", "commit.gpgsign", "false"]);

        std::fs::write(repo.join("tracked.txt"), "tracked content\n").unwrap();
        run(&["add", "tracked.txt"]);
        run(&["commit", "-m", "initial commit"]);
        std::fs::write(repo.join(".env"), "SECRET=abc\n").unwrap();

        let head = String::from_utf8(run(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        (temp, repo, worktrees, head)
    }

    #[test]
    fn worktree_create_gives_clean_worktree_without_untracked_env() {
        let (_temp, repo, worktrees, base_commit) = setup_temp_repo();
        let os = OsCommands::new();

        let info = create_worktree(
            &os,
            &worktrees,
            &repo,
            "test-project",
            "task-123",
            1,
            "main",
            1_700_000_000,
        )
        .expect("worktree create failed");

        assert_eq!(info.base_commit, base_commit);
        assert!(info.branch.starts_with("idlemax/"));
        assert!(info.folder.join("tracked.txt").exists());
        assert!(!info.folder.join(".env").exists());

        let marker_path = marker_path_for(&info.folder);
        assert!(marker_path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&marker_path).unwrap().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn creating_same_task_twice_appends_run_id() {
        let (_temp, repo, worktrees, _base_commit) = setup_temp_repo();
        let os = OsCommands::new();

        let info1 = create_worktree(
            &os,
            &worktrees,
            &repo,
            "test-project",
            "task-repeat",
            1,
            "main",
            1_700_000_000,
        )
        .unwrap();

        let info2 = create_worktree(
            &os,
            &worktrees,
            &repo,
            "test-project",
            "task-repeat",
            2,
            "main",
            1_700_000_100,
        )
        .unwrap();

        assert_ne!(info1.branch, info2.branch);
        assert_ne!(info1.folder, info2.folder);
        assert!(info2.branch.ends_with("-r2"));
        assert!(info2.folder.display().to_string().ends_with("-r2"));
    }

    #[test]
    fn remove_refuses_dirty_worktree_and_removes_clean_one() {
        let (_temp, repo, worktrees, _base_commit) = setup_temp_repo();
        let os = OsCommands::new();

        let info = create_worktree(
            &os,
            &worktrees,
            &repo,
            "test-project",
            "task-dirty",
            1,
            "main",
            1_700_000_000,
        )
        .unwrap();

        std::fs::write(info.folder.join("uncommitted.txt"), "dirty").unwrap();

        let err = remove_worktree(&os, &worktrees, &repo, &info.folder).unwrap_err();
        match err {
            WorktreeError::Git(msg) => {
                assert!(
                    msg.contains("contains modified or untracked files"),
                    "unexpected: {msg}"
                );
            }
            other => panic!("expected WorktreeError::Git, got {other:?}"),
        }

        std::fs::remove_file(info.folder.join("uncommitted.txt")).unwrap();
        remove_worktree(&os, &worktrees, &repo, &info.folder).expect("remove clean failed");
        assert!(!info.folder.exists());

        delete_branch(&os, &worktrees, &repo, &info.folder, &info.branch)
            .expect("delete branch failed");
    }

    #[test]
    fn remove_refuses_parent_traversal_and_outside_paths() {
        let (_temp, repo, worktrees, _base_commit) = setup_temp_repo();
        let os = OsCommands::new();

        // 1. Outside folder
        let outside = repo.clone();
        let err = remove_worktree(&os, &worktrees, &repo, &outside).unwrap_err();
        assert!(matches!(err, WorktreeError::OutsideWorktreesDir(..)));

        // 2. Traversal with .. (worktrees/../repo)
        let traversal = worktrees.join("..").join("repo");
        let err_traversal = remove_worktree(&os, &worktrees, &repo, &traversal).unwrap_err();
        assert!(matches!(
            err_traversal,
            WorktreeError::OutsideWorktreesDir(..)
        ));

        // 3. Under worktrees dir but missing marker
        let fake_folder = worktrees.join("fake-proj").join("fake-task");
        std::fs::create_dir_all(&fake_folder).unwrap();
        let err_missing = remove_worktree(&os, &worktrees, &repo, &fake_folder).unwrap_err();
        assert!(matches!(err_missing, WorktreeError::MissingMarker(..)));
    }
}
