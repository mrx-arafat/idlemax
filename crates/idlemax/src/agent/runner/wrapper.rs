//! Adapter: picks the OS throttle wrapper for jobs and resolves programs on the agent's PATH.

use std::path::{Path, PathBuf};

use idlemax_core::job_run::{systemd_major, Wrapper};

/// Picks the strongest throttle this machine offers.
pub fn detect_wrapper() -> Wrapper {
    if cfg!(target_os = "macos") {
        macos_wrapper()
    } else if cfg!(target_os = "linux") {
        linux_wrapper()
    } else if cfg!(windows) {
        Wrapper::Direct
    } else {
        nice_wrapper()
    }
}

fn macos_wrapper() -> Wrapper {
    let (taskpolicy, nice) = (
        Path::new("/usr/sbin/taskpolicy"),
        Path::new("/usr/bin/nice"),
    );
    if taskpolicy.is_file() && nice.is_file() {
        Wrapper::Taskpolicy {
            taskpolicy: path_string(taskpolicy),
            nice: path_string(nice),
        }
    } else {
        nice_wrapper()
    }
}

fn linux_wrapper() -> Wrapper {
    let booted = Path::new("/run/systemd/system").is_dir();
    match which("systemd-run") {
        Some(run) if booted && (is_root() || user_bus_exists()) => Wrapper::SystemdScope {
            systemd: systemd_version(&run),
            systemd_run: path_string(&run),
            user: !is_root(),
        },
        _ => nice_wrapper(),
    }
}

/// Asks `systemd-run --version`; any failure gives 0, so no version-dependent switch is guessed.
fn systemd_version(systemd_run: &Path) -> u32 {
    std::process::Command::new(systemd_run)
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| systemd_major(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or(0)
}

fn nice_wrapper() -> Wrapper {
    match which("nice") {
        Some(nice) => Wrapper::Nice {
            nice: path_string(&nice),
            ionice: which("ionice").map(|p| path_string(&p)),
        },
        None => Wrapper::Direct,
    }
}

#[cfg(unix)]
fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

fn user_bus_exists() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| user_manager_socket_in(Path::new(&d)))
}

/// `systemd-run --user` talks to the user manager over the D-Bus session socket, or over the
/// manager's own private socket when there is no session bus (typical for ssh on a server).
fn user_manager_socket_in(runtime_dir: &Path) -> bool {
    ["bus", "systemd/private"]
        .iter()
        .any(|socket| runtime_dir.join(socket).exists())
}

/// Resolves a bare program name on the agent's PATH.
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
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::user_manager_socket_in;

    #[test]
    fn user_manager_is_found_by_either_socket_systemd_run_uses() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!user_manager_socket_in(dir.path()), "empty runtime dir");
        std::fs::create_dir(dir.path().join("systemd")).unwrap();
        std::fs::write(dir.path().join("systemd/private"), "").unwrap();
        assert!(user_manager_socket_in(dir.path()), "private socket only");
        let bus = tempfile::tempdir().unwrap();
        std::fs::write(bus.path().join("bus"), "").unwrap();
        assert!(user_manager_socket_in(bus.path()), "session bus only");
    }
}
