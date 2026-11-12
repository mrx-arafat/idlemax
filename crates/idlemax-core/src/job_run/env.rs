//! Domain: which variables of the agent's environment a job inherits. The agent may hold API
//! keys, tokens or an ssh-agent socket in its environment; a job is run unattended and from a
//! spec anyone with the token can write, so it gets only what a tool needs to find its home,
//! its login and its settings. Anything else goes in the spec's own `env`. Pure.

/// Env var every job and every worker pane gets with its own job id; it also marks the
/// process as launched by idlemax, the worker marker (A12) that `control::caller` checks.
pub const JOB_ID_ENV: &str = "IDLEMAX_JOB_ID";
/// Env var every job gets with the random tag of this attempt: the agent finds the job's
/// processes by it, so it must not be shared with another agent's job of the same id.
pub const JOB_TAG_ENV: &str = "IDLEMAX_JOB_TAG";

/// Names inherited as they are: where tools live and who runs them, locale and time zone, the
/// temp dir, the state dir of idlemax itself, the Claude and Codex login dirs, the XDG base
/// dirs, and the user-manager address `systemd-run --user` needs. All are paths or settings.
const INHERITED: [&str; 17] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TZ",
    "TMPDIR",
    "IDLEMAX_HOME",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// Locale variables are inherited by prefix (`LC_ALL`, `LC_CTYPE`, ...).
const INHERITED_PREFIXES: [&str; 1] = ["LC_"];

/// True when a job inherits the agent's variable `name`.
pub fn inherits(name: &str) -> bool {
    INHERITED.contains(&name) || INHERITED_PREFIXES.iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::inherits;

    #[test]
    fn jobs_inherit_paths_and_locale_but_never_secrets() {
        for yes in [
            "PATH",
            "HOME",
            "USER",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TMPDIR",
            "IDLEMAX_HOME",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            assert!(inherits(yes), "{yes}");
        }
        for no in [
            "IDLEMAX_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "SSH_AUTH_SOCK",
            "HTTPS_PROXY",
            "NODE_OPTIONS",
            "BASH_ENV",
            "LD_PRELOAD",
            "path",
            "LCX",
            "",
        ] {
            assert!(!inherits(no), "{no}");
        }
    }
}
