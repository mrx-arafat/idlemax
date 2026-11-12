//! Adapter: each worker in its own transient systemd scope on Linux (red team case 10). A
//! process the worker double-forks or `setsid`s leaves its process group but never its
//! cgroup, so a stop ends every process in the scope: SIGTERM, then SIGKILL after a grace
//! period. Without a systemd manager the process group alone is ended, as before.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use idlemax_core::job_run::Wrapper;
use idlemax_core::types::JobId;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// How long a stopped scope's processes get to end on SIGTERM before SIGKILL.
const GRACE: Duration = Duration::from_secs(10);
/// The name every worker scope starts with; the caller check knows a worker by it.
pub const UNIT_PREFIX: &str = idlemax_core::control::caller::WORKER_SCOPE_PREFIX;
/// What `systemd-run --user` needs to reach the manager, and the worker never gets, so it
/// cannot open a fresh unit outside its scope.
const BUS_VARS: [&str; 2] = ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"];
/// Makes each unit name new within this agent, whatever the job and attempt.
static UNITS: AtomicU64 = AtomicU64::new(0);

/// How to start a worker in a scope of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerScope {
    systemd_run: String,
    user: bool,
    env: PathBuf,
    /// Moves a running pid (a Herdr pane's shell) into a new scope; None without `busctl`.
    busctl: Option<PathBuf>,
}

impl WorkerScope {
    /// The scope runner this machine offers: the runner's systemd wrapper on Linux, else None.
    pub fn detect() -> Option<Self> {
        let env = crate::agent::runner::wrapper::which("env");
        match (crate::agent::runner::wrapper::detect_wrapper(), env) {
            (
                Wrapper::SystemdScope {
                    systemd_run, user, ..
                },
                Some(env),
            ) => Some(Self {
                systemd_run,
                user,
                env,
                busctl: crate::agent::runner::wrapper::which("busctl"),
            }),
            _ => {
                if cfg!(target_os = "linux") {
                    tracing::info!(
                        "no systemd manager: a stopped worker's processes are ended by their \
                         process group only"
                    );
                }
                None
            }
        }
    }

    /// `systemd-run [--user] --scope --quiet --collect --unit idlemax-w-<job>-<attempt>-… --
    /// env -u XDG_RUNTIME_DIR -u DBUS_SESSION_BUS_ADDRESS <program>`: the program runs in
    /// place in a new scope, its pid unchanged. The caller adds the program's arguments and
    /// environment; `systemd-run` gets the agent's bus variables, the program never.
    pub fn command(&self, job: JobId, attempt: u32, program: &Path) -> Command {
        let mut cmd = Command::new(&self.systemd_run);
        if self.user {
            cmd.arg("--user");
        }
        let n = UNITS.fetch_add(1, Ordering::SeqCst);
        let unit = format!("{UNIT_PREFIX}{job}-{attempt}-{}-{n}", std::process::id());
        cmd.args(["--scope", "--quiet", "--collect", "--unit", &unit, "--"])
            .arg(&self.env);
        for var in BUS_VARS {
            cmd.args(["-u", var]);
        }
        cmd.arg(program);
        cmd
    }

    /// A scope runner with fixed programs, for a test that only looks at the calls.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            systemd_run: "systemd-run".into(),
            user: true,
            env: "env".into(),
            busctl: Some("busctl".into()),
        }
    }

    /// The `busctl` call that moves the running `pid` into a new worker scope, its children
    /// following it: the Herdr host cannot start a pane's shell inside one.
    pub fn adopt(&self, job: JobId, attempt: u32, pid: u32) -> Option<(String, Vec<String>)> {
        let busctl = self.busctl.as_ref()?.to_string_lossy().into_owned();
        let n = UNITS.fetch_add(1, Ordering::SeqCst);
        let unit = format!(
            "{UNIT_PREFIX}{job}-{attempt}-{}-{n}.scope",
            std::process::id()
        );
        let mut args: Vec<String> = Vec::new();
        if self.user {
            args.push("--user".into());
        }
        let call = [
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            "ssa(sv)a(sa(sv))",
        ];
        args.extend(call.iter().map(|s| s.to_string()));
        args.push(unit);
        // Like `systemd-run --collect`: the unit goes once it ends, even after a kill.
        let tail = [
            "fail",
            "2",
            "CollectMode",
            "s",
            "inactive-or-failed",
            "PIDs",
            "au",
            "1",
        ];
        args.extend(tail.iter().map(|s| s.to_string()));
        args.extend([pid.to_string(), "0".into()]);
        Some((busctl, args))
    }

    /// The agent's bus variables `systemd-run` reads, to set after a cleared environment.
    pub fn bus_env(&self) -> Vec<(String, String)> {
        BUS_VARS
            .iter()
            .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
            .collect()
    }
}

/// The cgroup folder of `pid` when it is a worker scope, from `/proc/<pid>/cgroup`.
pub fn worker_cgroup(pid: u32) -> Option<PathBuf> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    let last = path.rsplit('/').next()?;
    (last.starts_with(UNIT_PREFIX) && last.ends_with(".scope"))
        .then(|| Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// SIGTERM to every process in the scope now, SIGKILL to what is left after the grace.
pub fn end(cgroup: PathBuf) {
    signal_all(&cgroup, Signal::SIGTERM);
    std::thread::spawn(move || {
        std::thread::sleep(GRACE);
        signal_all(&cgroup, Signal::SIGKILL);
    });
}

fn signal_all(cgroup: &Path, sig: Signal) {
    let Ok(procs) = std::fs::read_to_string(cgroup.join("cgroup.procs")) else {
        return;
    };
    for pid in procs.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
        let _ = kill(Pid::from_raw(pid), sig);
    }
}
