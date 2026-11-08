//! Adapter: spawns one job attempt as an OS process under the throttle wrapper.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use idlemax_core::job_run::{scope_unit, Wrapper};
use idlemax_core::types::{Job, UnixMs};
use idlemax_core::{Paths, TOKEN_ENV};

use super::{os, RunnerError};

/// Env var every job gets with its own id.
pub const JOB_ID_ENV: &str = "IDLEMAX_JOB_ID";

pub struct Launched {
    pub child: Child,
    pub pid: u32,
    pub log_path: PathBuf,
    /// Where this attempt's output starts in the log.
    pub log_offset: u64,
}

pub fn launch(
    paths: &Paths,
    wrapper: &Wrapper,
    job: &Job,
    started_ms: UnixMs,
) -> Result<Launched, RunnerError> {
    let spec = &job.spec;
    let (program, args) = spec
        .command
        .split_first()
        .ok_or(RunnerError::EmptyCommand)?;
    let mut argv = vec![resolve_program(program)?];
    argv.extend(args.iter().cloned());
    let cwd = resolve_cwd(spec.cwd.as_deref())?;
    let (log, log_path, log_offset) = open_log(paths, job, started_ms, wrapper.name())?;
    let full = wrapper.wrap(&argv, spec.mem_mb, &scope_unit(job.id, started_ms));
    let (exe, exe_args) = full.split_first().ok_or(RunnerError::EmptyCommand)?;
    let stderr = log.try_clone().map_err(io_err("clone log", &log_path))?;
    let mut cmd = Command::new(exe);
    cmd.args(exe_args)
        .current_dir(&cwd)
        .envs(&spec.env)
        .env_remove(TOKEN_ENV)
        .env(JOB_ID_ENV, job.id.to_string())
        .stdin(
            spec.stdin
                .as_ref()
                .map_or_else(Stdio::null, |_| Stdio::piped()),
        )
        .stdout(log)
        .stderr(stderr);
    isolate(&mut cmd);
    let mut child = cmd.spawn().map_err(io_err("spawn", Path::new(exe)))?;
    feed_stdin(&mut child, spec.stdin.as_deref())?;
    Ok(Launched {
        pid: child.id(),
        child,
        log_path,
        log_offset,
    })
}

/// Hands the job its input without waiting for it. `start` runs on the agent loop under the
/// runner lock, so a blocking write to a program that never reads would stop sampling, the
/// guard, the kill switch and the heartbeat for as long as the job lives. A thread does the
/// write; it ends when the input is written or the job's pipe closes (it exited or was killed).
fn feed_stdin(child: &mut Child, input: Option<&str>) -> Result<(), RunnerError> {
    let Some(input) = input else {
        return Ok(());
    };
    let mut stdin = child.stdin.take().ok_or(RunnerError::StdinUnavailable)?;
    let input = input.to_owned();
    let writer = std::thread::Builder::new()
        .name("job-stdin".into())
        .spawn(move || {
            // A job that exits without reading everything is not an error: the pipe just closes.
            if let Err(e) = stdin.write_all(input.as_bytes()) {
                tracing::debug!(error = %e, "job closed stdin before reading it all");
            }
        });
    if let Err(e) = writer {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunnerError::Stdin(e));
    }
    Ok(())
}

/// Own process group, so pause and kill reach every process the job starts.
#[cfg(unix)]
fn isolate(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn isolate(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(IDLE_PRIORITY_CLASS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

/// Bare names resolve on the agent's PATH now, so a job's own env PATH cannot swap
/// the allowlisted program. Absolute paths are used as given.
fn resolve_program(program: &str) -> Result<String, RunnerError> {
    let path = Path::new(program);
    if path.is_absolute() {
        return if path.is_file() {
            Ok(program.to_string())
        } else {
            Err(RunnerError::ProgramNotFound(program.to_string()))
        };
    }
    if path.components().count() > 1 {
        return Err(RunnerError::RelativeProgram(program.to_string()));
    }
    os::which(program)
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| RunnerError::ProgramNotFound(program.to_string()))
}

/// Spec cwd with `~` expanded; the user's home when unset.
fn resolve_cwd(cwd: Option<&str>) -> Result<PathBuf, RunnerError> {
    let home = || dirs::home_dir().ok_or(RunnerError::NoHome);
    let dir = match cwd {
        None | Some("~") => home()?,
        Some(c) => match c.strip_prefix("~/") {
            Some(rest) => home()?.join(rest),
            None => PathBuf::from(c),
        },
    };
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(RunnerError::BadCwd(dir))
    }
}

fn open_log(
    paths: &Paths,
    job: &Job,
    started_ms: UnixMs,
    wrapper: &str,
) -> Result<(File, PathBuf, u64), RunnerError> {
    let path = paths.job_log_file(job.id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err("create", dir))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(io_err("open", &path))?;
    let at = chrono::DateTime::from_timestamp_millis(started_ms)
        .map_or_else(|| started_ms.to_string(), |t| t.to_rfc3339());
    let header = format!(
        "=== idlemax job {} attempt {} started {at} via {wrapper} ===\n",
        job.id, job.attempts
    );
    file.write_all(header.as_bytes())
        .map_err(io_err("write", &path))?;
    let offset = file.metadata().map_err(io_err("stat", &path))?.len();
    Ok((file, path, offset))
}

fn io_err<'a>(
    what: &'static str,
    path: &'a Path,
) -> impl FnOnce(std::io::Error) -> RunnerError + 'a {
    move |source| RunnerError::Io {
        what,
        path: path.to_path_buf(),
        source,
    }
}
