//! Adapter: capability detection, which known tools are on PATH.

use std::ffi::OsStr;
use std::path::Path;

/// Tools a JobSpec may list in `requires`, plus the enforcement wrappers.
pub const KNOWN_TOOLS: &[&str] = &[
    // AI CLIs (official, owner's login only)
    "claude",
    "codex",
    "agy",
    "opencode",
    "gemini",
    // dev tooling used by the MVP jobs
    "git",
    "gh",
    "node",
    "npm",
    "npx",
    "bun",
    "python3",
    "uv",
    "cargo",
    "rustc",
    "docker",
    "rg",
    "jq",
    "sqlite3",
    "make",
    "semble",
    "codegraph",
    // enforcement wrappers
    "taskpolicy",
    "systemd-run",
    "ionice",
    "chrt",
    "nice",
    "caffeinate",
];

/// Names from `tools` that resolve to an executable file in `path_var`, in `tools` order.
pub fn detect_on_path(path_var: &OsStr, tools: &[&str]) -> Vec<String> {
    let dirs: Vec<_> = std::env::split_paths(path_var)
        .filter(|d| !d.as_os_str().is_empty())
        .collect();
    tools
        .iter()
        .filter(|t| dirs.iter().any(|d| is_executable_in(d, t)))
        .map(|t| t.to_string())
        .collect()
}

/// Detects KNOWN_TOOLS on the current process PATH.
pub fn detect() -> Vec<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    detect_on_path(&path, KNOWN_TOOLS)
}

#[cfg(unix)]
fn is_executable_in(dir: &Path, name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir.join(name))
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_in(dir: &Path, name: &str) -> bool {
    ["exe", "cmd", "bat"]
        .iter()
        .any(|ext| dir.join(format!("{name}.{ext}")).is_file())
}
