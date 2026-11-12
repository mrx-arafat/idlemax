//! Tests: the worker sandbox against a real bubblewrap, one attack per review finding. They
//! are ignored by default: `scripts/sandbox-live.sh` runs them in a throwaway container as a
//! plain uid with `HOME=/srv/home`, `IDLEMAX_LIVE_BWRAP` (a bwrap that works there) and
//! `IDLEMAX_LIVE_BIN` (the idlemax binary for the relay). No real CLI and no network: a shell
//! script stands in for the worker's CLI and writes what it managed to `report.txt`.

use std::path::{Path, PathBuf};
use std::process::Command;

use idlemax_core::worker::WorkerCli;

use super::{wrap, GitDirs, SandboxPlan};

/// The fake owner's world under `HOME`: a project with a bare remote, another repository,
/// secrets in many places and idlemax's state folder, plus one worker's worktree.
struct World {
    home: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    folder: PathBuf,
}

/// None outside the live container, so a plain `--ignored` run never touches a real home.
fn world() -> Option<World> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    std::env::var_os("IDLEMAX_LIVE_BWRAP")?;
    assert!(
        home.starts_with("/srv/"),
        "live tests run only in the container"
    );
    for dir in [
        "code",
        ".idlemax",
        ".ssh",
        ".config",
        ".claude",
        ".codex",
        "remote.git",
    ] {
        let _ = std::fs::remove_dir_all(home.join(dir));
    }
    let repo = home.join("code/app");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for kv in [["user.name", "O"], ["user.email", "o@x"]] {
        git(&repo, &["config", kv[0], kv[1]]);
    }
    write(&repo.join("file.txt"), "one\n");
    git(&repo, &["add", "file.txt"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    git(&home, &["init", "-q", "--bare", "remote.git"]);
    git(
        &repo,
        &["remote", "add", "origin", &show(&home.join("remote.git"))],
    );
    let secrets = [
        "code/other/secret.txt",
        ".ssh/id_ed25519",
        ".config/gcloud/credentials.db",
        ".config/opencode/auth.json",
        ".npmrc",
        ".bash_history",
        ".claude/.credentials.json",
        ".codex/auth.json",
    ];
    for s in secrets {
        write(&home.join(s), "SECRET\n");
    }
    write(
        &home.join(".claude.json"),
        r#"{"userID":"u","mcpServers":{"db":{}}}"#,
    );
    let state = home.join(".idlemax");
    write(&state.join("token"), "TOKEN\n");
    let folder = state.join("worktrees/app/t1");
    std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
    git(
        &repo,
        &["worktree", "add", "-q", "-b", "idlemax/t1", &show(&folder)],
    );
    Some(World {
        home,
        repo,
        state,
        folder,
    })
}

/// Runs `script` as the worker's CLI inside the sandbox, the way the headless host does, and
/// returns its report.
fn run_worker(w: &World, cli: WorkerCli, script: &str) -> String {
    let stub = w
        .home
        .parent()
        .unwrap()
        .join(format!("stub-{}", cli.as_str()));
    write(
        &stub,
        &format!("#!/bin/sh\nexec >report.txt 2>&1\n{script}\n"),
    );
    chmod_x(&stub);
    let plan = SandboxPlan {
        bwrap: env_path("IDLEMAX_LIVE_BWRAP"),
        idlemax_bin: env_path("IDLEMAX_LIVE_BIN"),
        root: w.state.clone(),
        hosts: vec!["localhost".into()],
        headless: true,
    };
    let dirs = GitDirs {
        common: w.repo.join(".git"),
        admin: w.repo.join(".git/worktrees/t1"),
        run: 3,
        branch: "idlemax/t1".into(),
        start: rev(&w.repo, "main"),
    };
    let wrapped = wrap(&plan, (7, 1), cli, &w.folder, &dirs).unwrap();
    let wrap = wrapped.wrap.unwrap();
    let status = Command::new(&wrap.program)
        .args(&wrap.args)
        .arg(&stub)
        .current_dir(&w.folder)
        .env_clear()
        .env("HOME", &w.home)
        .env("PATH", "/usr/bin:/bin")
        .status()
        .unwrap();
    super::release(1);
    let report = std::fs::read_to_string(w.folder.join("report.txt")).unwrap_or_default();
    assert!(status.success(), "the stub failed: {report}");
    report
}

fn rev(repo: &Path, name: &str) -> String {
    let out = Command::new("git")
        .args(["-C", &show(repo), "rev-parse", name])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_finding_1_a_worker_cannot_move_the_owners_main() {
    let Some(w) = world() else { return };
    let main = rev(&w.repo, "main");
    let report = run_worker(
        &w,
        WorkerCli::Claude,
        "git -c user.name=w -c user.email=w@x commit -q --allow-empty -m mine; \
         git update-ref refs/heads/main HEAD && echo MOVED-MAIN; \
         git update-ref refs/heads/other HEAD && echo MADE-OTHER; true",
    );
    assert_eq!(
        rev(&w.repo, "main"),
        main,
        "the owner's main moved: {report}"
    );
    assert!(rev(&w.repo, "refs/heads/other").len() != 40, "{report}");
    // Only the Run's own branch comes over, with the worker's commit.
    super::private_git::import(&w.state, 3, &w.folder, "idlemax/t1").unwrap();
    let tip = rev(&w.repo, "idlemax/t1");
    assert_ne!(tip, main, "{report}");
    assert_eq!(rev(&w.repo, &format!("{tip}^")), main);
    assert!(rev(&w.repo, "refs/heads/other").len() != 40, "{report}");
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} in {}", dir.display());
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn chmod_x(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap())
}

fn show(p: &Path) -> String {
    p.display().to_string()
}
