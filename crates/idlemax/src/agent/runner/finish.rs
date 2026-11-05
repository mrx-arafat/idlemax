//! Adapter: closes one finished attempt: log footer, inbox note, terminal JobUpdate.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use idlemax_core::job_run::inbox::{self, RunReport};
use idlemax_core::job_run::{self, ExitInfo, Outcome, RunClock, StopReason};
use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{DeviceId, Job, UnixMs};
use idlemax_core::Paths;
use tracing::warn;

/// Output kept for the inbox note, the rate-limit scan and `JobUpdate.output_tail`.
pub const OUTPUT_TAIL_BYTES: u64 = 64 * 1024;

/// One ended attempt, as the runner tracked it.
pub struct Ended {
    pub job: Job,
    pub exit: ExitInfo,
    pub stop: Option<StopReason>,
    pub clock: RunClock,
    pub active_ms: i64,
    pub log_path: PathBuf,
    pub log_offset: u64,
}

pub fn finish(
    paths: &Paths,
    device: &DeviceId,
    wrapper: &str,
    ended: Ended,
    now_ms: UnixMs,
) -> JobUpdate {
    let outcome = job_run::outcome(ended.exit, ended.stop.as_ref());
    let output = read_tail(&ended.log_path, ended.log_offset).unwrap_or_else(|e| {
        warn!(job = ended.job.id, error = %e, "cannot read job log");
        String::new()
    });
    append_footer(&ended.log_path, &outcome);
    let report = RunReport {
        job: &ended.job,
        device,
        outcome: &outcome,
        started_ms: ended.clock.started_ms(),
        finished_ms: now_ms,
        active_ms: ended.active_ms,
        wrapper,
        log_path: &ended.log_path.to_string_lossy(),
        output: &output,
        utc_offset_secs: chrono::Local::now().offset().local_minus_utc(),
    };
    let output_path = write_note(paths, &report);
    JobUpdate {
        // Stamped by the agent outbox.
        seq: 0,
        id: ended.job.id,
        device: device.clone(),
        status: outcome.status,
        at_ms: now_ms,
        exit_code: outcome.exit_code,
        error: outcome.error,
        output_path: output_path.map(|p| p.to_string_lossy().into_owned()),
        rate_limited: job_run::rate_limited(ended.job.spec.cost, &output),
        output_tail: (!output.is_empty()).then_some(output),
        // The runner never decides owner stops; the agent sets it on local stop_all kills.
        owner_stop: false,
    }
}

/// Up to `OUTPUT_TAIL_BYTES` of this attempt's output, cut to whole lines.
fn read_tail(path: &Path, offset: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = offset.max(len.saturating_sub(OUTPUT_TAIL_BYTES));
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = match (start > offset, text.find('\n')) {
        (true, Some(i)) => &text[i + 1..],
        _ => &text[..],
    };
    Ok(text.to_string())
}

fn append_footer(path: &Path, outcome: &Outcome) {
    let exit = outcome
        .exit_code
        .map_or_else(|| "none".to_string(), |c| c.to_string());
    let line = format!("\n=== idlemax: {} (exit {exit}) ===\n", outcome.status);
    let result = OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = result {
        warn!(path = %path.display(), error = %e, "cannot append job log footer");
    }
}

/// Writes the note atomically (temp file, then rename) so readers never see half a note.
fn write_note(paths: &Paths, report: &RunReport<'_>) -> Option<PathBuf> {
    let dir = paths.inbox_dir();
    let name = inbox::file_name(
        report.job.id,
        &report.job.spec.name,
        report.finished_ms,
        report.utc_offset_secs,
    );
    let (tmp, path) = (dir.join(format!(".{name}.tmp")), dir.join(&name));
    let result = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&tmp, inbox::render(report)))
        .and_then(|()| std::fs::rename(&tmp, &path));
    match result {
        Ok(()) => Some(path),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cannot write inbox note");
            None
        }
    }
}
