//! Domain: the bubblewrap arguments of one sandboxed worker (A13, Spec 6 Task 2): the root
//! read-only, the secrets hidden, only the worktree, its git objects and refs, a temp dir and
//! the sandbox's Claude home writable, no network but the relay to the agent's proxy, and one
//! PID namespace that dies with bwrap. Pure.

/// Where the agent's pieces appear inside the sandbox.
pub const RUN_DIR: &str = "/run/idlemax";
pub const PROXY_SOCKET: &str = "/run/idlemax/proxy.sock";
pub const IDLEMAX_BIN: &str = "/run/idlemax/idlemax";
pub const CLAUDE_DIR: &str = "/run/idlemax/claude";
pub const RELAY_READY: &str = "/tmp/.idlemax-relay-ready";

/// Variables the sandbox never passes on: the agent socket and the user bus and runtime dir,
/// through which `systemd-run --user` would start a process outside the sandbox.
const UNSET: [&str; 3] = [
    "SSH_AUTH_SOCK",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
];

/// The sandbox's Claude home: idlemax's own folder, and the owner's credentials file bound
/// into it so a token refresh updates the owner's login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeHome {
    pub dir: String,
    pub credentials: Option<String>,
}

/// The facts of one launch the adapter found; every path is absolute and, for the hidden
/// and git paths, exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fence {
    pub worktree: String,
    /// The writable parts of the git common dir: `objects`, `refs`, `logs`, `packed-refs`
    /// and the worktree's own `worktrees/<name>`; never `config` or `hooks`.
    pub git_writable: Vec<String>,
    /// A host folder bound at `/tmp`.
    pub tmp: String,
    pub hidden_dirs: Vec<String>,
    pub hidden_files: Vec<String>,
    /// Bound read-only after the writable binds, inside them: what the worker must not change
    /// in a folder it may otherwise write.
    pub read_only: Vec<String>,
    /// Variables set inside, such as the private `GIT_DIR` of the worker's commits.
    pub env: Vec<(String, String)>,
    pub claude: Option<ClaudeHome>,
    pub proxy_socket: String,
    pub relay_port: u16,
    pub idlemax_bin: String,
}

/// The arguments after `bwrap`, up to the relay script's `$0`; the caller appends the CLI's
/// absolute path and its arguments, which the script `exec`s.
pub fn bwrap_args(f: &Fence) -> Vec<String> {
    let mut a = Argv::default();
    a.push(&[
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/run",
    ]);
    for dir in &f.hidden_dirs {
        a.push(&["--tmpfs", dir]);
    }
    for file in &f.hidden_files {
        a.push(&["--ro-bind", "/dev/null", file]);
    }
    a.bind(&f.worktree, &f.worktree);
    for path in &f.git_writable {
        a.bind(path, path);
    }
    a.bind(&f.tmp, "/tmp");
    if let Some(home) = &f.claude {
        a.bind(&home.dir, CLAUDE_DIR);
        if let Some(cred) = &home.credentials {
            a.bind(cred, &format!("{CLAUDE_DIR}/.credentials.json"));
        }
    }
    for path in &f.read_only {
        a.push(&["--ro-bind", path, path]);
    }
    a.bind(&f.proxy_socket, PROXY_SOCKET);
    a.push(&["--ro-bind", &f.idlemax_bin, IDLEMAX_BIN]);
    a.push(&[
        "--unshare-net",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
    ]);
    // Its own session in every host: no TIOCSTI into the pane's shell (CVE-2017-5226).
    a.push(&["--die-with-parent", "--new-session"]);
    let proxy = format!("http://127.0.0.1:{}", f.relay_port);
    for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
        a.push(&["--setenv", name, &proxy]);
    }
    a.push(&["--setenv", "NO_PROXY", "", "--setenv", "no_proxy", ""]);
    a.push(&["--setenv", "TMPDIR", "/tmp"]);
    if f.claude.is_some() {
        a.push(&["--setenv", "CLAUDE_CONFIG_DIR", CLAUDE_DIR]);
    }
    for (name, value) in &f.env {
        a.push(&["--setenv", name, value]);
    }
    for name in UNSET {
        a.push(&["--unsetenv", name]);
    }
    a.push(&[
        "--chdir",
        &f.worktree,
        "--",
        "/bin/sh",
        "-c",
        &relay_script(f.relay_port),
    ]);
    a.push(&[IDLEMAX_BIN]);
    a.0
}

/// Starts the relay in the background (a non-interactive shell leaves it deaf to Ctrl-C),
/// waits up to 5 s for its ready file, then becomes the CLI. `$0` is the idlemax binary.
fn relay_script(port: u16) -> String {
    format!(
        "\"$0\" sandbox-relay --socket {PROXY_SOCKET} --port {port} --ready {RELAY_READY} & \
         i=0; while [ ! -e {RELAY_READY} ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done; \
         exec \"$@\""
    )
}

#[derive(Default)]
struct Argv(Vec<String>);

impl Argv {
    fn push(&mut self, items: &[&str]) {
        self.0.extend(items.iter().map(|s| s.to_string()));
    }

    fn bind(&mut self, src: &str, dest: &str) {
        self.push(&["--bind", src, dest]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence() -> Fence {
        Fence {
            worktree: "/s/worktrees/app/t1".into(),
            git_writable: vec![
                "/code/app/.git/objects".into(),
                "/code/app/.git/refs".into(),
                "/code/app/.git/worktrees/t1".into(),
            ],
            tmp: "/s/sandbox/tmp/7-1".into(),
            hidden_dirs: vec!["/h/.ssh".into(), "/s".into()],
            hidden_files: vec!["/h/.git-credentials".into()],
            read_only: vec!["/s/sandbox/git/3/config".into()],
            env: vec![("GIT_DIR".into(), "/s/sandbox/git/3".into())],
            claude: Some(ClaudeHome {
                dir: "/s/sandbox/claude".into(),
                credentials: Some("/h/.claude/.credentials.json".into()),
            }),
            proxy_socket: "/s/sandbox/proxy-7-1.sock".into(),
            relay_port: 3128,
            idlemax_bin: "/usr/local/bin/idlemax".into(),
        }
    }

    #[test]
    fn the_argv_hides_the_secrets_before_it_binds_the_worktree_back() {
        let argv = bwrap_args(&fence());
        let line = argv.join(" ");
        let expected = "--ro-bind / / --dev /dev --proc /proc --tmpfs /run \
            --tmpfs /h/.ssh --tmpfs /s --ro-bind /dev/null /h/.git-credentials \
            --bind /s/worktrees/app/t1 /s/worktrees/app/t1 \
            --bind /code/app/.git/objects /code/app/.git/objects \
            --bind /code/app/.git/refs /code/app/.git/refs \
            --bind /code/app/.git/worktrees/t1 /code/app/.git/worktrees/t1 \
            --bind /s/sandbox/tmp/7-1 /tmp \
            --bind /s/sandbox/claude /run/idlemax/claude \
            --bind /h/.claude/.credentials.json /run/idlemax/claude/.credentials.json \
            --ro-bind /s/sandbox/git/3/config /s/sandbox/git/3/config \
            --bind /s/sandbox/proxy-7-1.sock /run/idlemax/proxy.sock \
            --ro-bind /usr/local/bin/idlemax /run/idlemax/idlemax \
            --unshare-net --unshare-pid --unshare-ipc --unshare-uts --die-with-parent --new-session \
            --setenv HTTPS_PROXY http://127.0.0.1:3128 --setenv HTTP_PROXY http://127.0.0.1:3128 \
            --setenv https_proxy http://127.0.0.1:3128 --setenv http_proxy http://127.0.0.1:3128 \
            --setenv NO_PROXY  --setenv no_proxy  --setenv TMPDIR /tmp \
            --setenv CLAUDE_CONFIG_DIR /run/idlemax/claude --setenv GIT_DIR /s/sandbox/git/3 \
            --unsetenv SSH_AUTH_SOCK --unsetenv DBUS_SESSION_BUS_ADDRESS --unsetenv XDG_RUNTIME_DIR \
            --chdir /s/worktrees/app/t1 -- /bin/sh -c";
        assert!(line.starts_with(expected), "{line}");
        let tail = &argv[argv.len() - 2..];
        assert_eq!(tail[1], IDLEMAX_BIN, "$0 of the script is the relay binary");
        let script = &tail[0];
        assert!(script.contains("sandbox-relay --socket /run/idlemax/proxy.sock --port 3128"));
        assert!(script.ends_with("exec \"$@\""), "{script}");
    }

    #[test]
    fn codex_gets_no_claude_home_and_never_a_writable_config_or_hooks() {
        let f = Fence {
            claude: None,
            ..fence()
        };
        let argv = bwrap_args(&f);
        assert!(!argv.iter().any(|a| a == "CLAUDE_CONFIG_DIR"));
        let mut writable = argv.windows(2).filter(|w| w[0] == "--bind").map(|w| &w[1]);
        assert!(!writable.any(|a| a.ends_with("/hooks") || a.ends_with("/config")));
    }
}
