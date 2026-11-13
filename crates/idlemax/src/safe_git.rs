//! Adapter: the argv of every git command idlemax runs on a project repository. A worktree
//! shares the repository's `.git/config` and `hooks/` with the owner's checkout, so a worker
//! could set `core.fsmonitor` or write a hook there; these options keep idlemax's own git
//! from running either, as the owner or as the agent.

use std::path::Path;

/// Put before the git subcommand: no fsmonitor command, no hook, never into a submodule or
/// through the file protocol a submodule would use, and no attributes file of the user's
/// (review finding N1: a worker's branch may hold a gitlink whose repository has a filter).
/// With `ENV`, the system config stays out too.
pub const SAFE: [&str; 12] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "submodule.recurse=false",
    "-c",
    "diff.ignoreSubmodules=all",
    "-c",
    "protocol.file.allow=never",
    "-c",
    "core.attributesFile=/dev/null",
];

/// The environment of every git idlemax runs on a project repository.
pub const ENV: [(&str, &str); 1] = [("GIT_CONFIG_NOSYSTEM", "1")];

/// The argv after `git`: the safe options, `-C <dir>`, then `args`.
pub fn args(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = SAFE.iter().map(|s| s.to_string()).collect();
    all.extend(["-C".to_string(), dir.display().to_string()]);
    all.extend(args.iter().map(|s| s.to_string()));
    all
}
