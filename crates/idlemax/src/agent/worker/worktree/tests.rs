//! Tests: worktrees made and removed with plain git in a temp repository.

use super::*;
use crate::agent::worker::cmd::OsCommands;

/// The real git, kept off the developer's global and system config; the code under test
/// does not set these, so they travel in `Cmd.env` from here only.
struct IsolatedGit(OsCommands);

impl Commands for IsolatedGit {
    fn run(&self, cmd: &Cmd) -> Result<Output, CmdError> {
        let mut cmd = cmd.clone();
        cmd.env
            .push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
        cmd.env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));
        self.0.run(&cmd)
    }
}

fn git() -> IsolatedGit {
    IsolatedGit(OsCommands::new())
}

fn new<'a>(repo: &'a Path, task_id: &'a str, run_id: u64) -> NewWorktree<'a> {
    NewWorktree {
        repo,
        project: "test-project",
        task_id,
        run_id,
        base_branch: "main",
    }
}

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
    let os = git();

    let info = create_worktree(&os, &worktrees, &new(&repo, "task-123", 1), 1_700_000_000)
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
    let os = git();

    let info1 = create_worktree(
        &os,
        &worktrees,
        &new(&repo, "task-repeat", 1),
        1_700_000_000,
    )
    .unwrap();

    let info2 = create_worktree(
        &os,
        &worktrees,
        &new(&repo, "task-repeat", 2),
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
    let os = git();

    let info =
        create_worktree(&os, &worktrees, &new(&repo, "task-dirty", 1), 1_700_000_000).unwrap();

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
    let os = git();

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

#[test]
fn a_failed_marker_write_removes_the_new_folder_and_branch() {
    let (_temp, repo, worktrees, _base_commit) = setup_temp_repo();
    let os = git();
    let t_slug = name::task_slug("task-rollback");
    let folder = name::worktree_dir(&worktrees, &name::slug("test-project"), &t_slug, None);
    // A directory where the marker file belongs makes its write fail.
    std::fs::create_dir_all(marker_path_for(&folder)).unwrap();

    let err = create_worktree(&os, &worktrees, &new(&repo, "task-rollback", 1), 1).unwrap_err();
    assert!(matches!(err, WorktreeError::Io(_)), "{err:?}");
    assert!(!folder.exists());
    let branch = name::branch_name(&t_slug, None);
    assert!(!branch_exists_in_repo(&os, &repo, &branch).unwrap());
}

#[test]
fn pane_env_blocks_a_push_to_a_local_bare_remote() {
    use idlemax_core::worker::launch::pane_env;

    let (temp, repo, worktrees, _base_commit) = setup_temp_repo();
    let os = git();
    let bare = temp.path().join("remote.git");
    let git_in = |dir: &Path, args: &[&str], env: &[(String, String)]| {
        let mut all = vec!["-C".to_string(), dir.display().to_string()];
        all.extend(args.iter().map(|s| s.to_string()));
        let cmd = Cmd {
            program: "git".into(),
            args: all,
            cwd: None,
            env: env.to_vec(),
            stdin: None,
            limit: Duration::from_secs(10),
            throttle: false,
            tag: None,
        };
        os.run(&cmd).unwrap()
    };
    let init = git_in(temp.path(), &["init", "--bare", "remote.git"], &[]);
    assert_eq!(init.code, Some(0), "{}", init.stderr);
    let bare_str = bare.display().to_string();
    git_in(&repo, &["remote", "add", "origin", &bare_str], &[]);

    let info = create_worktree(&os, &worktrees, &new(&repo, "task-push", 1), 1).unwrap();
    std::fs::write(info.folder.join("tracked.txt"), "changed\n").unwrap();
    let identity = [
        ("GIT_AUTHOR_NAME".to_string(), "W".to_string()),
        ("GIT_AUTHOR_EMAIL".to_string(), "w@test.local".to_string()),
        ("GIT_COMMITTER_NAME".to_string(), "W".to_string()),
        (
            "GIT_COMMITTER_EMAIL".to_string(),
            "w@test.local".to_string(),
        ),
    ];
    let env: Vec<(String, String)> = pane_env(7, 1, &["origin".to_string()], &[])
        .into_iter()
        .chain(identity)
        .collect();
    let commit = git_in(&info.folder, &["commit", "-am", "work"], &env);
    assert_eq!(commit.code, Some(0), "{}", commit.stderr);

    let push = git_in(
        &info.folder,
        &["push", "origin", "HEAD:refs/heads/leak"],
        &env,
    );
    assert_ne!(push.code, Some(0), "the push went through");
    let refs = git_in(&bare, &["for-each-ref"], &[]);
    assert!(refs.stdout.trim().is_empty(), "{}", refs.stdout);
}
