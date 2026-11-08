//! Adapter: per-OS process control for jobs (throttle detection, group signals, tree usage).

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use idlemax_core::job_run::Wrapper;
use idlemax_core::types::UnixMs;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sig {
    Stop,
    Cont,
    Term,
    Kill,
}

/// Signals a job's process group; its pgid is the root pid. A gone group is not an error.
#[cfg(unix)]
pub fn signal_group(pgid: u32, sig: Sig) -> io::Result<()> {
    use nix::sys::signal::{killpg, Signal};
    // pgid 0 is our own group and 1 is init: never signal those.
    let raw = i32::try_from(pgid).ok().filter(|p| *p > 1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal group {pgid}"),
        )
    })?;
    let signal = match sig {
        Sig::Stop => Signal::SIGSTOP,
        Sig::Cont => Signal::SIGCONT,
        Sig::Term => Signal::SIGTERM,
        Sig::Kill => Signal::SIGKILL,
    };
    match killpg(nix::unistd::Pid::from_raw(raw), signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(windows)]
pub fn signal_group(pid: u32, sig: Sig) -> io::Result<()> {
    match sig {
        // SHORTCUT: no pause on Windows, add NtSuspendProcess or a job object when a Windows device joins.
        Sig::Stop | Sig::Cont => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "pause is not supported on Windows",
        )),
        // SHORTCUT: taskkill tree kill, use a job object when a Windows device joins.
        Sig::Term | Sig::Kill => std::process::Command::new("taskkill")
            .args(["/PID", pid.to_string().as_str(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|_| ()),
    }
}

/// SIGKILLs processes left in a job's group after its root exited.
#[cfg(unix)]
pub fn kill_leftovers(pgid: u32) -> io::Result<()> {
    signal_group(pgid, Sig::Kill)
}

/// No-op: Windows reuses pids at once, so killing by pid after exit is unsafe.
#[cfg(windows)]
pub fn kill_leftovers(_pid: u32) -> io::Result<()> {
    Ok(())
}

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
            systemd_run: path_string(&run),
            user: !is_root(),
        },
        _ => nice_wrapper(),
    }
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

#[cfg(test)]
mod wrapper_tests {
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

/// Memory and CPU of a job's whole process tree.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub rss_bytes: u64,
    pub cpu_pct: f64,
}

/// OS process table, refreshed for memory and CPU only.
pub struct ProcTable {
    sys: System,
}

impl ProcTable {
    pub fn new() -> Option<Self> {
        System::new().ok().map(|sys| Self { sys })
    }

    pub fn refresh(&mut self) {
        let kind = ProcessRefreshKind::nothing()
            .with_memory()
            .with_cpu()
            .without_tasks();
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    }

    /// Process start time, for telling a recorded job from a reused pid.
    pub fn start_ms(&mut self, pid: u32) -> Option<UnixMs> {
        let pid = Pid::from_u32(pid);
        let kind = ProcessRefreshKind::nothing().without_tasks();
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, kind);
        let secs = self.sys.process(pid)?.start_time();
        i64::try_from(secs).ok().map(|s| s.saturating_mul(1000))
    }

    /// Tree usage per live root pid, from the last refresh.
    pub fn usages(&self, roots: &[u32]) -> HashMap<u32, Usage> {
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in self.sys.processes() {
            if let Some(parent) = process.parent() {
                children.entry(parent).or_default().push(*pid);
            }
        }
        roots
            .iter()
            .filter_map(|&root| self.tree(Pid::from_u32(root), &children).map(|u| (root, u)))
            .collect()
    }

    // SHORTCUT: walks the parent tree, so a job child that reparents (daemonizes) is not
    // counted; switch to a pgid or cgroup scan if a job ever does that.
    fn tree(&self, root: Pid, children: &HashMap<Pid, Vec<Pid>>) -> Option<Usage> {
        self.sys.process(root)?;
        let mut usage = Usage::default();
        let (mut stack, mut seen) = (vec![root], HashSet::new());
        while let Some(pid) = stack.pop() {
            if !seen.insert(pid) {
                continue;
            }
            if let Some(process) = self.sys.process(pid) {
                usage.rss_bytes += process.memory();
                usage.cpu_pct += f64::from(process.cpu_usage());
            }
            stack.extend(children.get(&pid).into_iter().flatten());
        }
        Some(usage)
    }
}
