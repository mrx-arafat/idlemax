//! Adapter: the job supervisor, `idlemax job-wrap --marker <file> -- <program> [args...]`.
//!
//! It sits between the OS throttle wrapper and the job, in the job's process group. When the job
//! ends it records the exit in the marker file before it exits itself, so an agent that died while
//! the job ran can still report the real exit. The agent sees the same exit status as it would from
//! the job directly: the code is passed through, and a signal is re-raised on this process.
//!
//! It runs once per job and counts in the job's memory, so it stays single-threaded and small:
//! `main` hands over before the async runtime or the CLI parser start.

/// The argv prefix that starts a supervisor, or None when the supervisor cannot be used here
/// (not unix, or this executable is gone, for example replaced by an upgrade).
#[cfg(unix)]
pub fn command() -> Option<Vec<String>> {
    // Unit tests run inside the test binary, which has no `job-wrap`; they opt in explicitly.
    if cfg!(test) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    exe.is_file()
        .then(|| vec![exe.to_string_lossy().into_owned(), SUBCOMMAND.into()])
}

#[cfg(not(unix))]
pub fn command() -> Option<Vec<String>> {
    None
}

/// The first argument that selects the supervisor.
#[cfg(unix)]
pub const SUBCOMMAND: &str = "job-wrap";

#[cfg(unix)]
pub use unix::run;

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io::{self, Write};
    use std::path::Path;
    use std::process::Command;

    use idlemax_core::job_run::{ExitInfo, ExitMarker};
    use nix::sys::signal::{raise, SigSet, Signal};

    use crate::agent::runner::exit_info;
    use crate::state::now_ms;

    const USAGE: &str =
        "usage: idlemax job-wrap --marker <file> -- <program> [args...] (the agent starts this)";

    /// Runs the job to its end and returns the exit code to leave with. A job that died of a
    /// signal does not return: the signal is re-raised on this process.
    pub fn run(args: &[OsString]) -> u8 {
        let Some((marker, argv)) = parse(args) else {
            eprintln!("{USAGE}");
            return 2;
        };
        let exit = match Command::new(&argv[0]).args(&argv[1..]).spawn() {
            Ok(mut job) => {
                // A stop the agent sends to the job's process group reaches this process as
                // well. Held, it waits until the job has ended and its exit is recorded; the job
                // gets the stop from the group. Held only now: a child spawned by std inherits
                // the signal mask (posix_spawn does not reset it), and the job must see stops.
                // A stop in the instant before this line ends the supervisor; the lease then
                // decides, as it would without one.
                if let Err(e) = stops().thread_block() {
                    eprintln!("idlemax job-wrap: cannot hold stop signals: {e}");
                }
                match job.wait() {
                    Ok(status) => exit_info(status),
                    Err(e) => failed(&format!("cannot wait for the job: {e}")),
                }
            }
            Err(e) => failed(&format!("cannot start {:?}: {e}", argv[0])),
        };
        record(marker, exit);
        leave(exit)
    }

    /// A job that could not run or be waited for reads as the shell's "command not found".
    fn failed(why: &str) -> ExitInfo {
        eprintln!("idlemax job-wrap: {why}");
        ExitInfo {
            code: Some(127),
            signal: None,
        }
    }

    fn parse(args: &[OsString]) -> Option<(&Path, &[OsString])> {
        let [flag, marker, sep, argv @ ..] = args else {
            return None;
        };
        (flag == "--marker" && sep == "--" && !argv.is_empty()).then_some((Path::new(marker), argv))
    }

    fn stops() -> SigSet {
        let mut set = SigSet::empty();
        set.add(Signal::SIGTERM);
        set.add(Signal::SIGINT);
        set
    }

    fn record(path: &Path, exit: ExitInfo) {
        let marker = ExitMarker::new(std::process::id(), now_ms(), exit);
        if let Err(e) = write_atomic(path, &marker.encode()) {
            eprintln!(
                "idlemax job-wrap: cannot record the exit in {}: {e}",
                path.display()
            );
        }
    }

    /// Temp file, fsync, rename: a reader sees the whole record or none.
    fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
        let tmp = path.with_extension("exit.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// Leaves the way the job did, so the agent reads the same status from this process.
    fn leave(exit: ExitInfo) -> u8 {
        let Some(signal) = exit.signal else {
            return exit.code.and_then(|c| u8::try_from(c).ok()).unwrap_or(1);
        };
        if let Ok(sig) = Signal::try_from(signal) {
            // The pending group stop, if any, then ends this process by that signal.
            let _ = stops().thread_unblock();
            let _ = raise(sig);
        }
        u8::try_from(128 + signal).unwrap_or(1)
    }
}
