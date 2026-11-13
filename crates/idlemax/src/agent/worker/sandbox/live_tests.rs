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
    write(
        &home.join(".codex/config.toml"),
        "model = \"gpt-5\"\n[mcp_servers.db]\ncommand = \"psql\"\n",
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
    run_worker_with(w, cli, script, None)
}

/// As `run_worker`, with a worker token file for Claude (review finding 6).
fn run_worker_with(w: &World, cli: WorkerCli, script: &str, token: Option<PathBuf>) -> String {
    run_as(w, 3, cli, script, token)
}

/// As `run_worker_with`, as Run `run`.
fn run_as(w: &World, run: i64, cli: WorkerCli, script: &str, token: Option<PathBuf>) -> String {
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
        claude_token: token,
    };
    let dirs = GitDirs {
        common: w.repo.join(".git"),
        admin: w.repo.join(".git/worktrees/t1"),
        run,
        branch: "idlemax/t1".into(),
        start: rev(&w.repo, "main"),
    };
    let wrapped = wrap(&plan, (7, 1), cli, &w.folder, &dirs).unwrap();
    let wrap = wrapped.wrap.unwrap();
    assert!(
        !wrap.args.iter().any(|a| a.contains("SECRET-TOKEN")),
        "never in argv"
    );
    let mut cmd = Command::new(&wrap.program);
    let status = cmd
        .args(&wrap.args)
        .arg(&stub)
        .current_dir(&w.folder)
        .env_clear()
        .env("HOME", &w.home)
        .env("PATH", "/usr/bin:/bin")
        .envs(
            wrap.token_file
                .as_ref()
                .map(|f| (super::TOKEN_VAR, super::read_token(f).unwrap())),
        )
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

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_finding_2_a_worker_cannot_delete_the_owners_objects() {
    let Some(w) = world() else { return };
    let report = run_worker(
        &w,
        WorkerCli::Claude,
        "objects=$(cat \"$GIT_DIR/objects/info/alternates\"); \
         find \"$objects\" -type f -path '*/[0-9a-f][0-9a-f]/*' -exec rm -f {} + && echo DELETED; \
         ls \"$objects\"/[0-9a-f][0-9a-f] | head -3; true",
    );
    let ok = Command::new("git")
        .args(["-C", &show(&w.repo), "cat-file", "-e", "main^{tree}"])
        .status()
        .unwrap()
        .success();
    assert!(ok, "the owner's objects were deleted: {report}");
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_finding_4_a_worker_cannot_redirect_idlemaxs_git() {
    let Some(w) = world() else { return };
    let pwned = w.home.join("pwned");
    let report = run_worker(
        &w,
        WorkerCli::Claude,
        &format!(
            "git --git-dir=.evil init -q; \
             git --git-dir=.evil config filter.x.clean \"sh -c 'touch {p}; cat'\"; \
             echo '* filter=x' > .gitattributes; \
             git --git-dir=.evil --work-tree=. add file.txt; echo changed >> file.txt; \
             printf 'gitdir: %s/.evil\\n' \"$PWD\" > .git && echo REWROTE-GITFILE; \
             echo /tmp > {admin}/commondir && echo REWROTE-COMMONDIR; true",
            p = pwned.display(),
            admin = w.repo.join(".git/worktrees/t1").display(),
        ),
    );
    // The check's own `git status`, outside the sandbox, as the agent.
    let _ = Command::new("git")
        .args(crate::safe_git::args(
            &w.folder,
            &["status", "--porcelain", "-z"],
        ))
        .output()
        .unwrap();
    assert!(
        !pwned.exists(),
        "idlemax's git ran the worker's filter: {report}"
    );
    assert!(!report.contains("REWROTE"), "{report}");
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_finding_3_the_home_is_an_allow_list() {
    let Some(w) = world() else { return };
    let report = run_worker(
        &w,
        WorkerCli::Claude,
        "for f in code/other/secret.txt .config/gcloud/credentials.db .npmrc .bash_history \
                  .config/opencode/auth.json .ssh/id_ed25519 .idlemax/token code/app/file.txt; do \
             cat \"$HOME/$f\" >/dev/null 2>&1 && echo \"READ $f\"; done; \
         ls /root >/dev/null 2>&1 && ls /root | head -1 | sed 's/^/ROOT /'; \
         echo scratch > \"$HOME/note\" && echo HOME-WRITABLE; \
         git -c user.name=w -c user.email=w@x commit -q --allow-empty -m ok && echo COMMITTED; true",
    );
    assert!(!report.contains("READ "), "{report}");
    assert!(report.contains("HOME-WRITABLE"), "a temp home: {report}");
    assert!(report.contains("COMMITTED"), "{report}");
    assert!(
        !w.home.join("note").exists(),
        "the temp home never reaches the owner's"
    );
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_finding_5_codex_logs_in_through_its_own_codex_home() {
    let Some(w) = world() else { return };
    let report = run_worker(
        &w,
        WorkerCli::Codex,
        "test -n \"$CODEX_HOME\" && echo HOME-SET; \
         grep -q SECRET \"$CODEX_HOME/auth.json\" && echo AUTH; \
         grep -q gpt-5 \"$CODEX_HOME/config.toml\" && echo CONFIG; \
         grep -q mcp_servers \"$CODEX_HOME/config.toml\" && echo MCP-LEAKED; \
         mkdir -p \"$CODEX_HOME/sessions\" && echo s > \"$CODEX_HOME/sessions/one\" && echo SAVED; true",
    );
    for want in ["HOME-SET", "AUTH", "CONFIG", "SAVED"] {
        assert!(report.contains(want), "{want}: {report}");
    }
    assert!(!report.contains("MCP-LEAKED"), "{report}");
    let kept = w.state.join("sandbox/codex/3/sessions/one");
    assert!(
        kept.is_file(),
        "a resumed Codex Run needs its sessions: {report}"
    );
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_finding_6_a_worker_token_replaces_the_owners_credentials() {
    let Some(w) = world() else { return };
    let probe = "test -n \"$CLAUDE_CODE_OAUTH_TOKEN\" && echo TOKEN-SET; \
                 test -e /run/idlemax/claude/.credentials.json && echo CRED-BOUND; \
                 echo x >> /run/idlemax/claude/.credentials.json 2>/dev/null && echo CRED-WRITABLE; true";
    // With the token: it is in the environment, and nothing of ~/.claude is bound.
    let file = w.home.parent().unwrap().join("worker-token");
    write(&file, "SECRET-TOKEN\n");
    std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let report = run_worker_with(&w, WorkerCli::Claude, probe, Some(file));
    assert!(report.contains("TOKEN-SET"), "{report}");
    assert!(!report.contains("CRED-BOUND"), "{report}");
    // Without it: the owner's credentials file, read-only.
    let report = run_worker(&w, WorkerCli::Claude, probe);
    assert!(
        report.contains("CRED-BOUND") && !report.contains("TOKEN-SET"),
        "{report}"
    );
    assert!(!report.contains("CRED-WRITABLE"), "{report}");
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_n3_a_codex_worker_never_writes_the_owners_auth() {
    let Some(w) = world() else { return };
    let owner = w.home.join(".codex/auth.json");
    let before = std::fs::read_to_string(&owner).unwrap();
    let report = run_worker(
        &w,
        WorkerCli::Codex,
        "grep -q SECRET \"$CODEX_HOME/auth.json\" && echo AUTH; \
         echo PWNED-AUTH > \"$CODEX_HOME/auth.json\" && echo OVERWROTE; \
         rm -f \"$CODEX_HOME/auth.json\"; true",
    );
    assert!(report.contains("AUTH"), "Codex still signs in: {report}");
    assert_eq!(std::fs::read_to_string(&owner).unwrap(), before, "{report}");
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_n4_each_run_gets_a_fresh_claude_home() {
    let Some(w) = world() else { return };
    let plant = "echo '{\"hooks\":{}}' > \"$CLAUDE_CONFIG_DIR/settings.json\" && echo PLANTED; \
                 printf '{\"projects\":{\"/other\":{\"mcpServers\":{\"x\":{}}}}}' \
                   > \"$CLAUDE_CONFIG_DIR/.claude.json\"; true";
    let look = "test -e \"$CLAUDE_CONFIG_DIR/settings.json\" && echo SETTINGS-LEFT; \
                grep -q mcpServers \"$CLAUDE_CONFIG_DIR/.claude.json\" && echo MCP-LEFT; \
                grep -q /other \"$CLAUDE_CONFIG_DIR/.claude.json\" && echo PROJECT-LEFT; \
                grep -q '\"userID\"' \"$CLAUDE_CONFIG_DIR/.claude.json\" && echo OWNER-KEYS; true";
    let report = run_as(&w, 3, WorkerCli::Claude, plant, None);
    assert!(report.contains("PLANTED"), "{report}");
    for run in [4, 3] {
        let report = run_as(&w, run, WorkerCli::Claude, look, None);
        assert!(report.contains("OWNER-KEYS"), "run {run}: {report}");
        for left in ["SETTINGS-LEFT", "MCP-LEFT", "PROJECT-LEFT"] {
            assert!(!report.contains(left), "run {run}: {left}: {report}");
        }
    }
}

#[test]
#[ignore = "live: scripts/sandbox-live.sh"]
fn live_n2_no_git_credential_store_is_readable() {
    let Some(w) = world() else { return };
    let outside = w.home.parent().unwrap().join("store-creds");
    write(&outside, "https://o:SECRET@github.com\n");
    write(
        &w.home.join(".config/git/credentials"),
        "https://o:SECRET@github.com\n",
    );
    write(
        &w.home.join(".config/git/config"),
        &format!(
            "[credential]\n\thelper = store --file {}\n[core]\n\tpager = less\n",
            outside.display()
        ),
    );
    let report = run_worker(
        &w,
        WorkerCli::Claude,
        &format!(
            "grep -q SECRET \"$HOME/.config/git/credentials\" && echo READ-XDG; \
             grep -q SECRET {} && echo READ-STORE; \
             grep -q pager \"$HOME/.config/git/config\" && echo GIT-CONFIG; true",
            outside.display()
        ),
    );
    assert!(!report.contains("READ-"), "{report}");
    assert!(
        report.contains("GIT-CONFIG"),
        "git's own settings still come back: {report}"
    );
}
