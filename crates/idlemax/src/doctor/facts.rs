//! Adapter: small local facts the doctor checks read (files, PATH, disk, commands).

use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a one-off command did.
#[derive(Debug)]
pub enum CmdOutcome {
    /// Ran to completion; `code` is None when a signal ended it.
    Ran {
        code: Option<i32>,
        stderr: String,
    },
    /// The program is not installed.
    Missing,
    Timeout,
    Spawn(std::io::Error),
}

/// Runs `cmd` with stdin closed and a hard deadline, so doctor never hangs.
pub async fn output(mut cmd: tokio::process::Command, timeout: Duration) -> CmdOutcome {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => CmdOutcome::Timeout,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => CmdOutcome::Missing,
        Ok(Err(e)) => CmdOutcome::Spawn(e),
        Ok(Ok(out)) => CmdOutcome::Ran {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        },
    }
}

pub fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Resolves a bare program name the way the job runner does: on this PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        candidates(&dir, name)
            .into_iter()
            .find(|p| is_executable(p))
    })
}

#[cfg(windows)]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![dir.join(name), dir.join(format!("{name}.exe"))]
}

#[cfg(not(windows))]
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![dir.join(name)]
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Free space on the filesystem holding the state dir, in MiB. Blocking.
pub fn disk_free_mib() -> Option<u64> {
    crate::agent::metrics::disk::read_root().map(|(free, _)| free / (1024 * 1024))
}

/// A home-relative path as `~/...`, so the report stays short.
pub fn short(path: &Path) -> String {
    match home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
