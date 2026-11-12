//! Adapter: per-OS process control for jobs (group signals, tree usage).

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;

pub use idlemax_core::job_run::Usage;
use idlemax_core::job_run::{could_belong, job_procs, orphan_procs, JobKey, JobProcs, ProcFact};
use idlemax_core::types::UnixMs;
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};
use tracing::warn;

use idlemax_core::job_run::JOB_TAG_ENV;

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

/// A process this old with an unreadable env is final: its env stays hidden.
const ENV_SETTLE_SECS: u64 = 5;

/// OS process table, refreshed for memory and CPU only.
pub struct ProcTable {
    sys: System,
    /// Attempt tag from each process's env, read once per process: pid to (start, tag).
    tags: HashMap<u32, (u64, Option<u64>)>,
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

    /// The variables of process `pid` as `name=value` pairs; None when the process is gone or
    /// its environment cannot be read (macOS hides it for some binaries), which the worker
    /// check treats as a failure.
    pub fn environ(&mut self, pid: u32) -> Option<Vec<(String, String)>> {
        let pid = Pid::from_u32(pid);
        let kind = ProcessRefreshKind::nothing()
            .with_environ(UpdateKind::Always)
            .without_tasks();
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), false, kind);
        let environ = self.sys.process(pid)?.environ();
        if environ.is_empty() {
            return None;
        }
        let pairs = environ.iter().filter_map(|entry| {
            let (name, value) = entry.to_str()?.split_once('=')?;
            Some((name.to_string(), value.to_string()))
        });
        Some(pairs.collect())
    }

    /// Resident memory of `pids` together, from the last refresh.
    pub fn rss_of(&self, pids: &[u32]) -> u64 {
        pids.iter()
            .filter_map(|&pid| self.sys.process(Pid::from_u32(pid)))
            .map(|p| p.memory())
            .sum()
    }

    /// The `n` processes with the most resident memory outside `exclude`, by name, from the
    /// last refresh; what a shed tells the owner instead of closing their apps (D14).
    pub fn top_memory(&self, n: usize, exclude: &[u32]) -> Vec<(String, u64)> {
        let mut all: Vec<(String, u64)> = self
            .sys
            .processes()
            .iter()
            .filter(|(pid, _)| !exclude.contains(&pid.as_u32()))
            .map(|(_, p)| (p.name().to_string_lossy().into_owned(), p.memory()))
            .collect();
        all.sort_by_key(|(_, rss)| std::cmp::Reverse(*rss));
        all.truncate(n);
        all
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
/// The attempt tag the runner put in a process's env (`IDLEMAX_JOB_TAG=<tag>`).
fn job_tag(environ: &[OsString]) -> Option<u64> {
    environ.iter().find_map(|entry| {
        let value = entry
            .to_str()?
            .strip_prefix(JOB_TAG_ENV)?
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

#[cfg(all(test, unix))]
mod environ_tests {
    #[test]
    fn environ_reads_a_childs_variables() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .env("IDLEMAX_ENV_PROBE", "seen")
            .spawn()
            .unwrap();
        let mut table = super::ProcTable::new().unwrap();
        let mut vars = None;
        for _ in 0..50 {
            vars = table.environ(child.id());
            if vars.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        let vars = vars.expect("environment read");
        assert!(vars
            .iter()
            .any(|(k, v)| k == "IDLEMAX_ENV_PROBE" && v == "seen"));
    }
}
