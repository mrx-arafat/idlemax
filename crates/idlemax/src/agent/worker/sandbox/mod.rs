//! Adapter: the worker sandbox of A13 (Spec 6): the bwrap probe, the facts of one launch's
//! fence, the sandbox's Claude home, the Herdr shim, the allow-list proxy the agent serves for
//! each sandboxed worker, the relay inside the sandbox, and the check that the CLI really runs
//! in another mount namespace.

#[cfg(test)]
mod live_tests;
pub mod private_git;
pub mod proxy;
pub mod relay;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use idlemax_core::types::JobId;
use idlemax_core::worker::sandbox::bwrap::{bwrap_args, ClaudeHome, Fence};
use idlemax_core::worker::sandbox::claude_home::sandbox_claude_json;
use idlemax_core::worker::WorkerCli;

use super::host::Wrap;
use proxy::Proxy;

/// The relay's port inside the sandbox's own network namespace, so it never meets another.
const RELAY_PORT: u16 = 3128;
/// How long the probe may take.
const PROBE_LIMIT: Duration = Duration::from_secs(5);
/// A Unix socket path longer than this cannot be bound (108 bytes with the terminator).
const MAX_SOCKET_PATH: usize = 100;

/// Folders that hold homes; each becomes an empty tmpfs (review finding 3).
const HOMES: [&str; 4] = ["/home", "/root", "/mnt", "/media"];

/// What of the owner's home a worker may read: toolchains and their caches, and git's own
/// settings. Never a credential (`~/.cargo/credentials*` stays out: only cargo's subfolders).
const TOOLCHAINS: [&str; 22] = [
    ".local/bin",
    ".local/lib",
    ".local/share/claude",
    ".local/share/mise",
    ".local/share/pnpm",
    ".cargo/bin",
    ".cargo/registry",
    ".cargo/git",
    ".cargo/config.toml",
    ".rustup",
    ".nvm",
    ".npm-global",
    ".bun",
    ".deno",
    ".pyenv",
    ".volta",
    ".asdf",
    ".sdkman",
    "go",
    ".gitconfig",
    ".config/git",
    ".config/mise",
];

/// The proxies of the sandboxed workers that run now, by attempt tag.
static PROXIES: Mutex<Option<HashMap<u64, Proxy>>> = Mutex::new(None);

/// What a sandboxed launch needs beyond the launch itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlan {
    /// The bwrap the probe found.
    pub bwrap: PathBuf,
    /// The state dir: hidden, and where the sandbox's own files live.
    pub root: PathBuf,
    pub hosts: Vec<String>,
    /// The idlemax binary the relay inside the sandbox runs.
    pub idlemax_bin: PathBuf,
    /// The headless host, which appends the CLI itself; a pane gets the shim.
    pub headless: bool,
}

/// The git dirs of the worktree: the shared common dir and the worktree's own admin dir.
pub struct GitDirs {
    pub common: PathBuf,
    pub admin: PathBuf,
    /// The Run, its branch and the commit the branch starts at, for its private git dir.
    pub run: i64,
    pub branch: String,
    pub start: String,
}

/// One launch's sandbox: the wrap for the headless host, or the env pairs that put the Herdr
/// shim first on the pane's `PATH`.
pub struct Wrapped {
    pub wrap: Option<Wrap>,
    pub env: Vec<(String, String)>,
}

/// The bwrap that may run workers here, or why none can.
pub fn probe() -> Result<PathBuf, String> {
    let bwrap = find_on_path("bwrap").ok_or("bwrap is not installed")?;
    let args = ["--ro-bind", "/", "/", "--unshare-net", "--unshare-pid"];
    let child = Command::new(&bwrap)
        .args(args)
        .args(["--die-with-parent", "/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("bwrap cannot start: {e}"))?;
    let out = wait_limited(child)?;
    match out.status.success() {
        true => Ok(bwrap),
        false => Err(format!(
            "bwrap cannot make its namespaces: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn wait_limited(mut child: std::process::Child) -> Result<std::process::Output, String> {
    let started = Instant::now();
    while child.try_wait().map_err(|e| e.to_string())?.is_none() {
        if started.elapsed() > PROBE_LIMIT {
            let _ = child.kill();
            return Err("bwrap did not answer within 5 s".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().map_err(|e| e.to_string())
}

/// Builds the fence of one launch, starts its proxy and returns how the host runs the CLI.
pub fn wrap(
    plan: &SandboxPlan,
    (job, tag): (JobId, u64),
    cli: WorkerCli,
    folder: &Path,
    git: &GitDirs,
) -> Result<Wrapped, String> {
    let io = |what: &str, e: std::io::Error| format!("the sandbox could not {what}: {e}");
    let sandbox = plan.root.join("sandbox");
    let tmp = sandbox.join("tmp").join(format!("{job}-{tag}"));
    crate::state::create_private_dir(&tmp).map_err(|e| io("make its temp dir", e))?;
    let private = private_git::dir_of(&plan.root, git.run);
    private_git::prepare(&private, &git.common, &git.admin, &git.branch, &git.start)
        .map_err(|e| format!("the sandbox could not make its git dir: {e}"))?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let claude = match cli {
        WorkerCli::Claude => Some(claude_home(&sandbox, home.as_deref(), folder)?),
        _ => None,
    };
    let socket = socket_path(&sandbox, job, tag);
    let proxy = Proxy::start(
        &socket,
        job,
        plan.hosts.clone(),
        proxy::dial_tcp,
        proxy::Limits::default(),
    )
    .map_err(|e| io("start its proxy", e))?;
    let fence = Fence {
        worktree: show(folder),
        git_writable: git_writable(&private),
        read_only: read_only(folder, &private),
        env: vec![
            ("GIT_DIR".into(), show(&private)),
            ("GIT_WORK_TREE".into(), show(folder)),
        ],
        tmp: show(&tmp),
        hidden_dirs: hidden_dirs(&plan.root),
        home: home.as_deref().map(show),
        visible: visible(home.as_deref(), &git.common, cli),
        hidden_files: Vec::new(),
        claude,
        proxy_socket: show(&socket),
        relay_port: RELAY_PORT,
        idlemax_bin: show(&plan.idlemax_bin),
    };
    let wrap = Wrap {
        program: plan.bwrap.clone(),
        args: bwrap_args(&fence),
    };
    keep(tag, proxy);
    if plan.headless {
        return Ok(Wrapped {
            wrap: Some(wrap),
            env: Vec::new(),
        });
    }
    let env = shim(&sandbox, (job, tag), cli, &wrap)?;
    Ok(Wrapped { wrap: None, env })
}

/// Stops the proxy of the attempt `tag`, when it has one.
pub fn release(tag: u64) {
    let mut map = PROXIES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(map) = map.as_mut() {
        map.remove(&tag);
    }
}

fn keep(tag: u64, proxy: Proxy) {
    let mut map = PROXIES.lock().unwrap_or_else(PoisonError::into_inner);
    map.get_or_insert_with(HashMap::new).insert(tag, proxy);
}

/// The sandbox's Claude home with its `.claude.json` written for this worktree.
fn claude_home(sandbox: &Path, home: Option<&Path>, folder: &Path) -> Result<ClaudeHome, String> {
    let dir = sandbox.join("claude");
    crate::state::create_private_dir(&dir).map_err(|e| e.to_string())?;
    let read = |p: &Path| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    };
    let owner = home
        .and_then(|h| read(&h.join(".claude.json")))
        .unwrap_or_default();
    let file = dir.join(".claude.json");
    let json = sandbox_claude_json(&owner, read(&file).as_ref(), &show(folder));
    std::fs::write(&file, json.to_string()).map_err(|e| e.to_string())?;
    let credentials = home
        .map(|h| h.join(".claude/.credentials.json"))
        .filter(|p| p.is_file())
        .map(|p| show(&p));
    Ok(ClaudeHome {
        dir: show(&dir),
        credentials,
    })
}

/// What the worker's git writes: its private git dir alone. Never the owner's refs, logs,
/// `packed-refs` (review finding 1) or objects (finding 2), and never the worktree's admin dir
/// (finding 4): the worker reads the owner's objects through its alternate.
fn git_writable(private: &Path) -> Vec<String> {
    vec![show(private)]
}

/// Read-only inside the writable worktree and private git dir: the worktree's `.git` file, so
/// the worker cannot point idlemax's own git at a repository it made (finding 4), and the
/// private dir's settings.
fn read_only(folder: &Path, private: &Path) -> Vec<String> {
    let mut out = vec![show(&folder.join(".git"))];
    out.extend(private_git::read_only(private).iter().map(|p| show(p)));
    out
}

/// Every folder of homes, and the state dir when it lies outside them.
fn hidden_dirs(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = HOMES
        .iter()
        .map(Path::new)
        .filter(|p| p.is_dir())
        .map(show)
        .collect();
    if root.is_dir() && !HOMES.iter().any(|h| root.starts_with(h)) {
        out.push(show(root));
    }
    out
}

/// The allow-list bound back read-only: the toolchains under `home`, the owner's git objects
/// (the worker's alternate) and the folder the CLI is installed in.
fn visible(home: Option<&Path>, common: &Path, cli: WorkerCli) -> Vec<String> {
    let mut out: Vec<PathBuf> = home
        .into_iter()
        .flat_map(|h| TOOLCHAINS.iter().map(move |t| h.join(t)))
        .filter(|p| p.exists())
        .collect();
    out.push(common.join("objects"));
    let installed = find_on_path(cli.as_str())
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| p.parent().map(Path::to_path_buf));
    out.extend(installed.filter(|dir| !out.iter().any(|v| dir.starts_with(v))));
    out.iter().map(|p| show(p)).collect()
}

fn socket_path(sandbox: &Path, job: JobId, tag: u64) -> PathBuf {
    let near = sandbox.join(format!("{job}.sock"));
    match near.as_os_str().len() <= MAX_SOCKET_PATH {
        true => near,
        false => std::env::temp_dir().join(format!("idlemax-{job}-{tag}.sock")),
    }
}

/// Writes the shim that runs the CLI under bwrap and returns the pane's `PATH` pair with the
/// shim's folder first. The shim takes its own folder off `PATH` and finds the CLI there, as
/// a bare start in that pane would.
fn shim(
    sandbox: &Path,
    (job, tag): (JobId, u64),
    cli: WorkerCli,
    wrap: &Wrap,
) -> Result<Vec<(String, String)>, String> {
    let dir = sandbox.join("shims").join(format!("{job}-{tag}"));
    crate::state::create_private_dir(&dir).map_err(|e| e.to_string())?;
    let mut words = vec![quote(&show(&wrap.program))];
    words.extend(wrap.args.iter().map(|a| quote(a)));
    let name = cli.as_str();
    let body = format!(
        "#!/bin/sh\nPATH=${{PATH#{dir}:}}\nexport PATH\n\
         cli=$(command -v {name}) || {{ echo '{name} is not on PATH' >&2; exit 127; }}\n\
         exec {} \"$cli\" \"$@\"\n",
        words.join(" "),
        dir = show(&dir),
    );
    let file = dir.join(name);
    std::fs::write(&file, body).map_err(|e| e.to_string())?;
    set_executable(&file)?;
    let path = std::env::var("PATH").unwrap_or_default();
    Ok(vec![("PATH".into(), format!("{}:{path}", show(&dir)))])
}

#[cfg(unix)]
fn set_executable(file: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn set_executable(_file: &Path) -> Result<(), String> {
    Ok(())
}

/// One shell word in single quotes.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn show(p: &Path) -> String {
    p.display().to_string()
}

/// True when `root` or a process under it runs in another mount namespace than this agent:
/// the CLI really is inside the sandbox.
pub fn runs_sandboxed(root: u32) -> bool {
    let ns = |pid: &str| std::fs::read_link(format!("/proc/{pid}/ns/mnt")).ok();
    let Some(own) = ns("self") else {
        return false;
    };
    let mut todo = vec![root];
    let mut seen = 0;
    while let Some(pid) = todo.pop() {
        seen += 1;
        if seen > 4096 {
            return false;
        }
        if ns(&pid.to_string()).is_some_and(|n| n != own) {
            return true;
        }
        todo.extend(children(pid));
    }
    false
}

fn children(pid: u32) -> Vec<u32> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten();
    tasks
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
        .flat_map(|text| {
            text.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect::<Vec<u32>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_with_a_quote_stays_one_shell_word() {
        assert_eq!(quote("a'b c"), "'a'\\''b c'");
    }

    #[test]
    fn the_agent_itself_does_not_run_sandboxed() {
        assert!(!runs_sandboxed(std::process::id()));
    }
}
