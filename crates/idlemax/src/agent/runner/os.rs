//! Adapter: per-OS process control for jobs (throttle detection, group signals, tree usage).

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

pub use idlemax_core::job_run::Usage;
use idlemax_core::job_run::{
    could_belong, job_procs, orphan_procs, supports_scope_oom_policy, JobKey, JobProcs, ProcFact,
    Wrapper,
};
use idlemax_core::types::{JobId, UnixMs};
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};
use tracing::warn;

use super::launch::JOB_ID_ENV;

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
    let raw = signalable(pgid, "group")?;
    allow_gone(
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(raw), unix_signal(sig)),
        pgid,
    )
}

#[cfg(unix)]
fn signal_pid(pid: u32, sig: Sig) -> io::Result<()> {
    let raw = signalable(pid, "process")?;
    allow_gone(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), unix_signal(sig)),
        pid,
    )
}

/// pid 0 is our own group and 1 is init: never signal those.
#[cfg(unix)]
fn signalable(id: u32, what: &str) -> io::Result<i32> {
    i32::try_from(id).ok().filter(|p| *p > 1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal {what} {id}"),
        )
    })
}

#[cfg(unix)]
fn unix_signal(sig: Sig) -> nix::sys::signal::Signal {
    use nix::sys::signal::Signal;
    match sig {
        Sig::Stop => Signal::SIGSTOP,
        Sig::Cont => Signal::SIGCONT,
        Sig::Term => Signal::SIGTERM,
        Sig::Kill => Signal::SIGKILL,
    }
}

/// A signal to something already gone is not an error. macOS answers EPERM, not ESRCH, while
/// the only processes left are unreaped zombies (a group whose leader exited and was not yet
/// waited on), so there EPERM counts as gone unless `root`, the signalled pid or group leader,
/// is still alive: then it is a real denial.
#[cfg(unix)]
fn allow_gone(result: nix::Result<()>, root: u32) -> io::Result<()> {
    use nix::errno::Errno;
    match result {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(Errno::EPERM) if cfg!(target_os = "macos") && !alive(root) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// True while `pid` is a live (not zombie) process. Fails closed: when the process table
/// cannot be read the process counts as alive.
#[cfg(unix)]
pub(super) fn alive(pid: u32) -> bool {
    let pid = Pid::from_u32(pid);
    let Some(mut sys) = System::new().ok() else {
        return true;
    };
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .is_some_and(|p| p.status() != ProcessStatus::Zombie)
}

/// Signals each pid, best effort: a member can exit between the scan and the signal, and
/// macOS answers EPERM for a process that is exiting. Failures are logged, never fatal.
#[cfg(unix)]
pub fn signal_pids(pids: &[u32], sig: Sig) {
    for &pid in pids {
        if let Err(e) = signal_pid(pid, sig) {
            warn!(pid, signal = ?sig, error = %e, "cannot signal job member");
        }
    }
}

/// No-op: Windows kills by tree, which needs a live root.
#[cfg(windows)]
pub fn signal_pids(_pids: &[u32], _sig: Sig) {}

/// Signals everything a job runs: its group (the result), then the members that left the
/// group (`setsid` daemons), which are best effort.
#[cfg(unix)]
pub fn signal_job(pgid: u32, outside: &[u32], sig: Sig) -> io::Result<()> {
    let group = signal_group(pgid, sig);
    signal_pids(outside, sig);
    group
}

/// Windows kills the whole tree with `taskkill /T`, so there is no outside set.
#[cfg(windows)]
pub fn signal_job(pid: u32, _outside: &[u32], sig: Sig) -> io::Result<()> {
    signal_group(pid, sig)
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

/// SIGKILLs processes left over after a job's root exited: its group and `outside`.
#[cfg(unix)]
pub fn kill_leftovers(pgid: u32, outside: &[u32]) -> io::Result<()> {
    signal_job(pgid, outside, Sig::Kill)
}

/// No-op: Windows reuses pids at once, so killing by pid after exit is unsafe.
#[cfg(windows)]
pub fn kill_leftovers(_pid: u32, _outside: &[u32]) -> io::Result<()> {
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
            oom_kill: scope_oom_policy_supported(&run),
            systemd_run: path_string(&run),
            user: !is_root(),
        },
        _ => nice_wrapper(),
    }
}

/// Asks `systemd-run --version`; any failure means no, so the property is never guessed.
fn scope_oom_policy_supported(systemd_run: &Path) -> bool {
    std::process::Command::new(systemd_run)
        .arg("--version")
        .output()
        .is_ok_and(|out| {
            out.status.success() && supports_scope_oom_policy(&String::from_utf8_lossy(&out.stdout))
        })
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

/// A process this old with an unreadable env is final: its env stays hidden.
const ENV_SETTLE_SECS: u64 = 5;

/// OS process table, refreshed for memory and CPU only.
pub struct ProcTable {
    sys: System,
    /// Job id from each process's env, read once per process: pid to (start, tag).
    tags: HashMap<u32, (u64, Option<JobId>)>,
}

impl ProcTable {
    pub fn new() -> Option<Self> {
        System::new().ok().map(|sys| Self {
            sys,
            tags: HashMap::new(),
        })
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

    /// Processes of each live job, keyed by root pid, from the last refresh.
    pub fn usages(&mut self, jobs: &[JobKey]) -> HashMap<u32, JobProcs> {
        let facts = self.tagged_facts(jobs);
        jobs.iter()
            .filter_map(|job| job_procs(job, &facts).map(|found| (job.root, found)))
            .collect()
    }

    /// What a job left running after its agent died (see `orphan_procs`). Refreshes first.
    pub fn orphans(&mut self, job: &JobKey, root_trusted: bool) -> JobProcs {
        self.refresh();
        let facts = self.tagged_facts(std::slice::from_ref(job));
        orphan_procs(job, &facts, root_trusted)
    }

    /// The process table with each process's job tag filled in.
    fn tagged_facts(&mut self, jobs: &[JobKey]) -> Vec<ProcFact> {
        let mut facts = self.facts();
        self.read_tags(jobs, &facts);
        for fact in &mut facts {
            fact.tag = self
                .tags
                .get(&fact.pid)
                .and_then(|&(start, tag)| tag.filter(|_| start == fact.start_secs));
        }
        facts
    }

    fn facts(&self) -> Vec<ProcFact> {
        self.sys
            .processes()
            .iter()
            .filter(|(_, process)| process.status() != ProcessStatus::Zombie)
            .map(|(pid, process)| ProcFact {
                pid: pid.as_u32(),
                ppid: process.parent().map(Pid::as_u32),
                pgid: group_of(pid.as_u32()),
                start_secs: process.start_time(),
                tag: None,
                rss_bytes: process.memory(),
                cpu_pct: f64::from(process.cpu_usage()),
            })
            .collect()
    }

    /// Reads the job-id env of processes that started since a job did and sit outside
    /// every job group. Each process is read once, so a steady table costs nothing.
    fn read_tags(&mut self, jobs: &[JobKey], facts: &[ProcFact]) {
        let live: HashMap<u32, u64> = facts.iter().map(|f| (f.pid, f.start_secs)).collect();
        self.tags
            .retain(|pid, (start, _)| live.get(pid) == Some(start));
        let Some(since_ms) = jobs.iter().map(|j| j.started_ms).min() else {
            return;
        };
        let roots: HashSet<u32> = jobs.iter().map(|j| j.root).collect();
        let fresh: Vec<Pid> = facts
            .iter()
            .filter(|f| !self.tags.contains_key(&f.pid))
            .filter(|f| could_belong(f.start_secs, since_ms))
            .filter(|f| f.pgid.is_none_or(|g| !roots.contains(&g)))
            .map(|f| Pid::from_u32(f.pid))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let kind = ProcessRefreshKind::nothing()
            .with_environ(UpdateKind::OnlyIfNotSet)
            .without_tasks();
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::Some(&fresh), false, kind);
        for pid in fresh {
            if let Some(process) = self.sys.process(pid) {
                let tag = job_tag(process.environ());
                // A young process may be caught mid-exec with no env yet: read it again.
                let settled = process.run_time() >= ENV_SETTLE_SECS;
                if tag.is_some() || !process.environ().is_empty() || settled {
                    self.tags.insert(pid.as_u32(), (process.start_time(), tag));
                }
            }
        }
    }
}

// SHORTCUT: macOS hides the env of Apple-signed binaries, so a setsid daemon built from one
// is not attributed to its job; use kqueue NOTE_TRACK or Endpoint Security if a job does that.
/// The job id the runner put in a process's env (`IDLEMAX_JOB_ID=<id>`).
fn job_tag(environ: &[OsString]) -> Option<JobId> {
    environ.iter().find_map(|entry| {
        let value = entry
            .to_str()?
            .strip_prefix(JOB_ID_ENV)?
            .strip_prefix('=')?;
        value.parse().ok()
    })
}

/// Process group of `pid`; `None` when the process is gone or on Windows.
#[cfg(unix)]
fn group_of(pid: u32) -> Option<u32> {
    let raw = i32::try_from(pid).ok()?;
    let group = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(raw))).ok()?;
    u32::try_from(group.as_raw()).ok()
}

#[cfg(not(unix))]
fn group_of(_pid: u32) -> Option<u32> {
    None
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
