//! Adapter: the argv of every git command idlemax runs on a project repository. A worktree
//! shares the repository's `.git/config` and `hooks/` with the owner's checkout, so a worker
//! could set `core.fsmonitor` or write a hook there; these options keep idlemax's own git
//! from running either, as the owner or as the agent.

use std::path::Path;

/// Put before the git subcommand: no fsmonitor command, no hook.
pub const SAFE: [&str; 4] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
];

/// The argv after `git`: the safe options, `-C <dir>`, then `args`.
pub fn args(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = SAFE.iter().map(|s| s.to_string()).collect();
    all.extend(["-C".to_string(), dir.display().to_string()]);
    all.extend(args.iter().map(|s| s.to_string()));
    all
}
