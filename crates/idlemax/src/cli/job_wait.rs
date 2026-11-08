//! Delivery: the waiting half of `idlemax job add --wait`: poll the coordinator, print how
//! the job ended with the end of its log, and report the job's outcome as the exit code.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use anyhow::Result;
use idlemax_core::job_run::inbox::tail;
use idlemax_core::job_wait::{exit_code, poll_delay, TIMED_OUT_EXIT};
use idlemax_core::types::{Job, JobStatus};
use idlemax_core::Paths;
use serde::Serialize;

use super::client::ClientError;
use super::job::ExitWith;
use super::{out, print, rows, ApiClient};

/// Log lines printed with the result.
const LOG_LINES: usize = 20;
/// Only the end of a log is read, however large it grew.
const MAX_LOG_BYTES: u64 = 64 * 1024;
/// How long a coordinator restart may take before the wait gives up.
const UNREACHABLE_GRACE: Duration = Duration::from_secs(120);

/// Where the end of the job's log can be found.
#[derive(Debug, PartialEq)]
enum Log {
    Lines(Vec<String>),
    /// The job ran on another device, which has the log.
    On(String),
    None,
}

/// The `--json` shape of a waited job: `log_tail` is null when the log is not on this device.
#[derive(Serialize)]
struct Report<'a> {
    job: &'a Job,
    log_tail: Option<&'a [String]>,
    timed_out: bool,
}

/// Waits for `job`, prints the outcome and returns Ok only when the job succeeded.
pub async fn run(
    c: &ApiClient,
    paths: &Paths,
    job: Job,
    timeout: Option<Duration>,
    json: bool,
) -> Result<()> {
    let started = Instant::now();
    let (job, timed_out) = poll(c, job, timeout).await?;
    if timed_out {
        let waited = started.elapsed();
        let report = Report {
            job: &job,
            log_tail: None,
            timed_out,
        };
        out(json, &report, |_| timeout_text(&job, waited));
        return Err(ExitWith(TIMED_OUT_EXIT).into());
    }
    let log = log_of(paths, &job);
    let lines = match &log {
        Log::Lines(lines) => Some(lines.as_slice()),
        _ => None,
    };
    let report = Report {
        job: &job,
        log_tail: lines,
        timed_out,
    };
    out(json, &report, |_| result_text(&job, &log));
    match exit_code(job.status) {
        0 => Ok(()),
        code => Err(ExitWith(code).into()),
    }
}

/// Polls until the job is finished, or `timeout` passes (then the flag is true). Status
/// changes go to stderr. A coordinator that is briefly down is waited out.
async fn poll(c: &ApiClient, mut job: Job, timeout: Option<Duration>) -> Result<(Job, bool)> {
    let started = Instant::now();
    let mut announced = job.status;
    let mut down_since: Option<Instant> = None;
    for polls in 0.. {
        if job.status.is_terminal() {
            break;
        }
        let mut pause = poll_delay(polls);
        if let Some(limit) = timeout {
            let left = limit.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Ok((job, true));
            }
            pause = pause.min(left);
        }
        tokio::time::sleep(pause).await;
        match c.job(job.id).await {
            Ok(latest) => {
                down_since = None;
                job = latest;
            }
            Err(ClientError::Unreachable { .. })
                if down_since.get_or_insert_with(Instant::now).elapsed() < UNREACHABLE_GRACE =>
            {
                eprintln!("coordinator unreachable, still waiting for job {}", job.id);
            }
            Err(e) => return Err(super::hint(e, super::COORD_DOWN)),
        }
        // The result follows a finished job, so only the steps before it are reported.
        if job.status != announced && !job.status.is_terminal() {
            announced = job.status;
            eprintln!("{}", progress_text(&job));
        }
    }
    Ok((job, false))
}

/// The end of the job's log if it is on this device.
fn log_of(paths: &Paths, job: &Job) -> Log {
    match read_end(&paths.job_log_file(job.id)) {
        Some(text) => Log::Lines(tail(&text, LOG_LINES).lines().map(str::to_string).collect()),
        None => job.device.clone().map_or(Log::None, Log::On),
    }
}

fn read_end(path: &std::path::Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(MAX_LOG_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A status change while waiting; a retry says why the job is back in the queue.
fn progress_text(job: &Job) -> String {
    match (&job.last_error, job.status) {
        (Some(why), JobStatus::Queued) => format!("job {} queued again: {why}", job.id),
        _ => format!("job {} {}", job.id, job.status),
    }
}

fn result_text(job: &Job, log: &Log) -> String {
    let code = job.exit_code.map_or("none".to_string(), |c| c.to_string());
    let mut text = format!(
        "{}\nexit code: {code}\nran: {}",
        print::job_line(job),
        ran_text(job)
    );
    // "exit code N" would only repeat the line above.
    let repeats_code = |e: &str| job.exit_code.is_some_and(|c| e == format!("exit code {c}"));
    if let Some(error) = job.last_error.as_deref().filter(|e| !repeats_code(e)) {
        text.push_str(&format!("\nerror: {error}"));
    }
    match log {
        Log::Lines(lines) if lines.is_empty() => text.push_str("\nlog: empty"),
        Log::Lines(lines) => {
            text.push_str(&format!(
                "\nlog (last {LOG_LINES} lines):\n{}",
                lines.join("\n")
            ));
        }
        Log::On(device) => text.push_str(&format!(
            "\nlog: on device {device}; run `idlemax logs {}` there",
            job.id
        )),
        Log::None => text.push_str("\nlog: none recorded"),
    }
    text
}

fn ran_text(job: &Job) -> String {
    let (Some(start), Some(end)) = (job.started_ms, job.finished_ms) else {
        return "never started".into();
    };
    let ms = (end - start).max(0);
    let took = if ms < 10_000 {
        format!("{}.{}s", ms / 1000, (ms % 1000) / 100)
    } else {
        rows::dur(ms)
    };
    match &job.device {
        Some(device) => format!("{took} on {device}"),
        None => took,
    }
}

fn timeout_text(job: &Job, waited: Duration) -> String {
    let id = job.id;
    format!(
        "job {id} is still {} after waiting {}s: {}\n\
         it keeps its place: `idlemax why {id}` shows what it waits for, \
         `idlemax job cancel {id}` drops it",
        job.status,
        waited.as_secs(),
        job.spec.name
    )
}

#[cfg(test)]
mod tests;
