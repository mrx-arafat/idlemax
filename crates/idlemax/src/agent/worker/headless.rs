//! Adapter: the headless worker host, the second `WorkerHost` (dispatcher Spec 5 Task 8). It
//! starts the agent CLI as a child of the agent in its own process group, with the minimal
//! environment of a job plus the pairs of a pane, and writes the worker's output to the job
//! log that `idlemax logs` reads. It has no pane, so nothing can wait for input: the agent is
//! `idle` before its prompt, `working` while it lives after it and `done` once it exited.
//! A headless worker is a child of the agent, so it never outlives it. On Linux it runs in
//! a systemd scope of its own, so a stop also ends what it forked out of its group.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use idlemax_core::job_run::inherits;
use idlemax_core::types::JobId;
use idlemax_core::worker::report::MAX_END_TEXT_CHARS;
use idlemax_core::worker::WorkerCli;
use idlemax_core::{Paths, TOKEN_ENV};

use super::host::{AgentStatus, Handle, HostError, HostState, Launch, WorkerHost};
use super::scope::{self, WorkerScope};
use crate::agent::runner::os::{signal_group, Sig};

#[cfg(test)]
mod tests;

/// The last lines of the log that `end_text` keeps, before the character cut (A9).
const END_TEXT_LINES: usize = 40;
/// A finished worker's slot is kept this long, so a slow tick still reads its exit.
const KEEP_FINISHED: Duration = Duration::from_secs(600);
const FOOTER_PREFIX: &str = "=== idlemax: ";
/// How often the reaper looks for the child's exit.
const REAP_POLL: Duration = Duration::from_millis(50);

struct Slot {
    pid: u32,
    log: PathBuf,
    /// Where this attempt's output starts in the log, after its header.
    offset: u64,
    session: Option<String>,
    stdin: Mutex<Option<std::process::ChildStdin>>,
    sent: AtomicBool,
    stopping: Arc<AtomicBool>,
    /// The exit code, set by the reaper once the footer is in the log. The child is reaped
    /// only under this lock, so its pid cannot belong to another process while `stop` holds
    /// it and reads `None`.
    exit: Arc<Mutex<Option<i32>>>,
    finished: Arc<Mutex<Option<Instant>>>,
    /// The worker's scope cgroup, read once the child runs in it; None without a scope.
    cgroup: Arc<Mutex<Option<PathBuf>>>,
}

#[derive(Default)]
pub struct HeadlessHost {
    paths: Option<Paths>,
    /// Keyed by the handle's label, which is new for every start; a pid can come back.
    slots: Mutex<HashMap<String, Arc<Slot>>>,
    starts: AtomicU64,
    /// Starts each worker in a scope of its own; None where there is no systemd manager.
    scope: Option<WorkerScope>,
    /// The only directory a test build searches, so a test finds its stub CLI.
    #[cfg(test)]
    bin_dir: Option<PathBuf>,
}

impl HeadlessHost {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths: Some(paths),
            scope: WorkerScope::detect(),
            ..Default::default()
        }
    }

    /// A host that finds its CLI in `bin_dir` alone, for a test with a stub.
    #[cfg(test)]
    pub fn with_bin_dir(paths: Paths, bin_dir: PathBuf) -> Self {
        Self {
            bin_dir: Some(bin_dir),
            ..Self::new(paths)
        }
    }

    /// The slot of this very start: its label and its pid both match.
    fn slot(&self, handle: &Handle) -> Option<Arc<Slot>> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&handle.label)
            .filter(|s| s.pid.to_string() == handle.pane)
            .cloned()
    }

    /// Where the agent CLI is searched: `PATH`.
    #[cfg(not(test))]
    fn search_dirs(&self) -> Vec<PathBuf> {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect()
    }

    /// A test build searches its own directory alone, never `PATH`, so a test that forgot
    /// its stub can never start a real agent CLI.
    #[cfg(test)]
    fn search_dirs(&self) -> Vec<PathBuf> {
        self.bin_dir.iter().cloned().collect()
    }

    fn program(&self, cli: WorkerCli) -> Result<PathBuf, HostError> {
        let name = cli.as_str();
        self.search_dirs()
            .iter()
            .map(|d| d.join(name))
            .find(|p| p.is_file())
            .ok_or_else(|| failed(format!("{name} is not on the agent's PATH")))
    }
}

fn failed(why: String) -> HostError {
    HostError::Failed {
        call: "headless start".into(),
        why,
    }
}

/// The session id the CLI was given, as `--session-id <id>` or `--resume <id>`.
fn session_of(args: &[String]) -> Option<String> {
    let at = args
        .iter()
        .position(|a| a == "--session-id" || a == "--resume")?;
    args.get(at + 1).cloned()
}

/// The job log of this attempt, with the header the runner writes (`runner/launch.rs`) so
/// `idlemax logs --follow` finds where the attempt starts.
fn open_log(
    paths: &Paths,
    job: JobId,
    attempt: u32,
) -> Result<(std::fs::File, PathBuf, u64), HostError> {
    let path = paths.job_log_file(job);
    let io = |e: std::io::Error| failed(format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        crate::state::create_private_dir(dir).map_err(io)?;
    }
    let mut file = crate::state::private_file()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io)?;
    let now = chrono::Utc::now().to_rfc3339();
    let header =
        format!("=== idlemax job {job} attempt {attempt} started {now} via headless ===\n");
    file.write_all(header.as_bytes()).map_err(io)?;
    let offset = file.metadata().map_err(io)?.len();
    Ok((file, path, offset))
}

/// Appends the footer that ends the attempt for `idlemax logs --follow`.
fn write_footer(log: &Path, status: &str, code: i32) {
    let line = format!("\n{FOOTER_PREFIX}{status} (exit {code}) ===\n");
    let _ = std::fs::OpenOptions::new()
        .append(true)
        .open(log)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// The job's minimal environment (`job_run::inherits`), then the pane's pairs, never the token;
/// in a scope of its own when there is one. A sandboxed launch runs the wrapper with the CLI's
/// path after its arguments (A13), inside the same scope.
fn command(
    program: &Path,
    launch: &Launch,
    scope: Option<&WorkerScope>,
    token: Option<&str>,
) -> Command {
    use std::os::unix::process::CommandExt;
    let first = launch
        .wrap
        .as_ref()
        .map_or(program, |w| w.program.as_path());
    let mut cmd = match scope {
        Some(s) => s.command(launch.job, launch.attempt, first),
        None => Command::new(first),
    };
    if let Some(wrap) = &launch.wrap {
        cmd.args(&wrap.args).arg(program);
    }
    cmd.args(&launch.args)
        .current_dir(&launch.folder)
        .env_clear();
    cmd.envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(inherits)));
    cmd.envs(launch.env.iter().map(|(k, v)| (k, v)));
    if let Some(s) = scope {
        cmd.envs(s.bus_env());
    }
    // The worker token of a sandboxed Claude, in the environment only (review finding 6).
    if let Some(token) = token {
        cmd.env(super::sandbox::TOKEN_VAR, token);
    }
    cmd.env_remove(TOKEN_ENV).process_group(0);
    cmd
}

impl WorkerHost for HeadlessHost {
    fn start(&self, launch: &Launch) -> Result<Handle, HostError> {
        let paths = self
            .paths
            .as_ref()
            .ok_or_else(|| failed("no state directory".into()))?;
        let program = self.program(launch.cli)?;
        let token = match launch.wrap.as_ref().and_then(|w| w.token_file.as_deref()) {
            Some(file) => Some(super::sandbox::read_token(file).map_err(failed)?),
            None => None,
        };
        let (log, log_path, offset) = open_log(paths, launch.job, launch.attempt)?;
        let stderr = log
            .try_clone()
            .map_err(|e| failed(format!("{}: {e}", log_path.display())))?;
        // Under a scope `systemd-run` spawns whatever the program is, so check it here.
        if !executable(&program) {
            write_footer(&log_path, "failed", -1);
            return Err(failed(format!("{} is not executable", program.display())));
        }
        let spawned = command(&program, launch, self.scope.as_ref(), token.as_deref())
            .stdin(Stdio::piped())
            .stdout(log)
            .stderr(stderr)
            .spawn();
        let mut child = spawned.map_err(|e| {
            write_footer(&log_path, "failed", -1);
            failed(format!("{}: {e}", program.display()))
        })?;
        let pid = child.id();
        let slot = Arc::new(Slot {
            pid,
            log: log_path.clone(),
            offset,
            session: session_of(&launch.args),
            stdin: Mutex::new(child.stdin.take()),
            sent: AtomicBool::new(false),
            stopping: Arc::new(AtomicBool::new(false)),
            exit: Arc::new(Mutex::new(None)),
            finished: Arc::new(Mutex::new(None)),
            cgroup: Arc::new(Mutex::new(None)),
        });
        reap(child, &slot);
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.retain(|_, s| {
            let done = *s.finished.lock().unwrap_or_else(PoisonError::into_inner);
            done.is_none_or(|at| at.elapsed() < KEEP_FINISHED)
        });
        let start = self.starts.fetch_add(1, Ordering::SeqCst);
        let handle = Handle {
            pane: pid.to_string(),
            workspace: "headless".into(),
            label: format!("{} #{start}", launch.name),
        };
        slots.insert(handle.label.clone(), slot);
        Ok(handle)
    }

    fn state(&self, handle: &Handle) -> Result<HostState, HostError> {
        let gone = |exit_code| HostState {
            status: AgentStatus::Gone,
            session: None,
            shell_pid: None,
            foreground: Vec::new(),
            exit_code,
        };
        let Some(slot) = self.slot(handle) else {
            return Ok(gone(None));
        };
        let exit = *slot.exit.lock().unwrap_or_else(PoisonError::into_inner);
        let sent = slot.sent.load(Ordering::SeqCst);
        let status = match (exit, sent) {
            (Some(_), true) => AgentStatus::Done,
            // It ended before it was given anything to do: nothing is left to wait for.
            (Some(code), false) => return Ok(gone(Some(code))),
            (None, true) => AgentStatus::Working,
            (None, false) => AgentStatus::Idle,
        };
        Ok(HostState {
            status,
            session: slot.session.clone(),
            shell_pid: Some(slot.pid),
            foreground: Vec::new(),
            exit_code: exit,
        })
    }

    /// Writes the prompt to the child's standard input and closes it: print mode reads its
    /// prompt there to the end. The write is on a thread, so a child that never reads cannot
    /// hold the manager. A second send on one handle is an error.
    fn send(&self, handle: &Handle, text: &str) -> Result<(), HostError> {
        let slot = self
            .slot(handle)
            .ok_or_else(|| failed("no such worker".into()))?;
        if slot.sent.swap(true, Ordering::SeqCst) {
            return Err(HostError::AlreadySent {
                handle: handle.pane.clone(),
            });
        }
        let stdin = slot
            .stdin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(mut stdin) = stdin else {
            return Ok(());
        };
        let prompt = format!("{text}\n");
        std::thread::spawn(move || {
            // A child that exits without reading everything just closes the pipe.
            let _ = stdin.write_all(prompt.as_bytes());
        });
        Ok(())
    }

    /// SIGTERM to the child's process group. The manager's search by pid and attempt tag kills
    /// what is left after its grace period.
    fn stop(&self, handle: &Handle) -> Result<(), HostError> {
        let Some(slot) = self.slot(handle) else {
            return Ok(());
        };
        // Held while signalling: the reaper cannot reap the child, so its pid stays its own.
        let exit = slot.exit.lock().unwrap_or_else(PoisonError::into_inner);
        if exit.is_some() {
            return Ok(());
        }
        slot.stopping.store(true, Ordering::SeqCst);
        let cgroup = slot
            .cgroup
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(cgroup) = cgroup {
            scope::end(cgroup);
        }
        signal_group(slot.pid, Sig::Term).map_err(|e| HostError::Failed {
            call: "SIGTERM".into(),
            why: e.to_string(),
        })
    }

    fn matches(&self, handle: &Handle) -> Result<bool, HostError> {
        Ok(self.slot(handle).is_some_and(|s| {
            s.exit
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none()
        }))
    }

    /// The last 40 lines of this attempt's log, without the footer, cut to 2,000 characters.
    fn end_text(&self, handle: &Handle) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let slot = self.slot(handle)?;
        let mut file = std::fs::File::open(&slot.log).ok()?;
        file.seek(SeekFrom::Start(slot.offset)).ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with(FOOTER_PREFIX))
            .collect();
        let tail = lines[lines.len().saturating_sub(END_TEXT_LINES)..].join("\n");
        let tail = tail.trim();
        let skip = tail.chars().count().saturating_sub(MAX_END_TEXT_CHARS);
        let kept: String = tail.chars().skip(skip).collect();
        (!kept.is_empty()).then_some(kept)
    }

    fn is_headless(&self) -> bool {
        true
    }
}

/// Waits for the child on a thread of its own, so it never stays a zombie. It reaps only
/// while it holds the exit lock, then writes the footer `idlemax logs --follow` stops on and
/// publishes the exit code before it lets go.
fn reap(mut child: std::process::Child, slot: &Slot) {
    let (log, stopping) = (slot.log.clone(), Arc::clone(&slot.stopping));
    let (exit, finished) = (Arc::clone(&slot.exit), Arc::clone(&slot.finished));
    let (cgroup, pid) = (Arc::clone(&slot.cgroup), slot.pid);
    std::thread::spawn(move || loop {
        let mut exit = exit.lock().unwrap_or_else(PoisonError::into_inner);
        // The scope's cgroup, once the child has moved into it.
        let mut known = cgroup.lock().unwrap_or_else(PoisonError::into_inner);
        if known.is_none() {
            *known = scope::worker_cgroup(pid);
        }
        drop(known);
        let code = match child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(exit_code(status)),
            Err(_) => Some(-1),
        };
        if let Some(code) = code {
            // Whatever the worker left running in its scope ends with it.
            let left = cgroup
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(left) = left {
                scope::end(left);
            }
            let status = match (stopping.load(Ordering::SeqCst), code) {
                (true, _) => "killed",
                (false, 0) => "succeeded",
                (false, _) => "failed",
            };
            write_footer(&log, status, code);
            *finished.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
            *exit = Some(code);
            return;
        }
        drop(exit);
        std::thread::sleep(REAP_POLL);
    });
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The exit code, or 128 plus the signal that ended it.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(-1)
}
