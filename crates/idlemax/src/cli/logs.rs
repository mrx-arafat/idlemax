//! Delivery: `idlemax logs` subcommand: prints job output or asks coordinator where it ran.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use idlemax_core::job_run::LogProgress;
use idlemax_core::types::JobId;

use super::out::toon::Node;
use super::{ApiClient, Out, Peer};

const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Runs `idlemax logs <job-id> [--tail N] [--follow]` on a local log file.
pub async fn run_logs_local(
    path: &Path,
    tail: Option<usize>,
    follow: bool,
    _out: &Out,
) -> Result<()> {
    if follow {
        follow_log(path, tail).await
    } else {
        print_log(path, tail)
    }
}

/// Runs `idlemax logs <job-id>` by asking the coordinator which device ran the job.
pub async fn run_logs_remote(client: &ApiClient, id: JobId, out: &Out) -> Result<()> {
    handle_missing_log(client, id, out).await
}

/// Print the current content of the log file, optionally tailing the last N lines.
fn print_log(path: &Path, tail: Option<usize>) -> Result<()> {
    let content =
        std::fs::read(path).with_context(|| format!("reading log at {}", path.display()))?;
    print_bytes(&content, tail)
}

/// Preserve job bytes, including non-UTF-8 output and partial final lines.
fn print_bytes(content: &[u8], tail: Option<usize>) -> Result<()> {
    let bytes = match tail {
        Some(0) => &[][..],
        Some(n) => {
            let body = content.strip_suffix(b"\n").unwrap_or(content);
            let start = body
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, b)| **b == b'\n')
                .nth(n - 1)
                .map_or(0, |(i, _)| i + 1);
            &content[start..]
        }
        None => content,
    };
    match write_chunk(bytes) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

/// Streams new bytes from `path` until the job exit footer is seen or interrupted.
async fn follow_log(path: &Path, tail: Option<usize>) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("opening log at {}", path.display()))?;
    let mut initial_content = Vec::new();
    file.read_to_end(&mut initial_content)?;

    print_bytes(&initial_content, tail)?;

    let mut progress = LogProgress::default();
    if progress.push(&initial_content) {
        return Ok(());
    }

    let mut buf = [0u8; 4096];
    loop {
        let n = file.read(&mut buf)?;
        if n > 0 {
            let chunk = &buf[..n];
            match write_chunk(chunk) {
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                other => other?,
            }
            if progress.push(chunk) {
                break;
            }
        } else {
            let meta = std::fs::metadata(path)?;
            let current_pos = file.stream_position()?;
            if meta.len() < current_pos {
                file.seek(SeekFrom::Start(0))?;
                progress = LogProgress::default();
                continue;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    Ok(())
}

fn write_chunk(chunk: &[u8]) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    stdout.write_all(chunk)?;
    stdout.flush()
}

/// When the local log file is absent, asks the coordinator which device ran the job.
async fn handle_missing_log(client: &ApiClient, id: JobId, out: &Out) -> Result<()> {
    let job = client
        .job(id)
        .await
        .map_err(|e| super::hint(e, Peer::Coordinator))?;
    let (text, note, hints) = match &job.device {
        Some(device) => (
            format!("job {id} ran on {device}; run this there"),
            format!("not on this device; it ran on {device}"),
            vec![format!("Run `idlemax logs {id}` on {device} to read it")],
        ),
        None => (
            format!("job {id} has not run yet (status: {})", job.status),
            "none yet, the job has not run".to_string(),
            vec![format!(
                "Run `idlemax why {id}` to see why it has not started"
            )],
        ),
    };
    out.emit_with(
        &job,
        |_| text.clone(),
        || {
            Ok(Node::obj([
                ("job", Node::Int(id)),
                ("status", Node::str(job.status.to_string())),
                ("log", Node::Str(note)),
            ]))
        },
        &hints,
    )
}
