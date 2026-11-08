//! Delivery: `idlemax logs` subcommand: prints job output or asks coordinator where it ran.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use idlemax_core::job_run::inbox::tail as tail_lines;
use idlemax_core::types::JobId;

use super::client::{ApiClient, ClientError};

const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Runs `idlemax logs <job-id> [--tail N] [--follow]` on a local log file.
pub async fn run_logs_local(path: &Path, tail: Option<usize>, follow: bool) -> Result<()> {
    if follow {
        follow_log(path, tail).await
    } else {
        print_log(path, tail)
    }
}

/// Runs `idlemax logs <job-id>` by asking the coordinator which device ran the job.
pub async fn run_logs_remote(client: &ApiClient, path: &Path, id: JobId) -> Result<()> {
    handle_missing_log(client, path, id).await
}

/// Print the current content of the log file, optionally tailing the last N lines.
fn print_log(path: &Path, tail: Option<usize>) -> Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading log at {}", path.display()))?;
    if let Some(n) = tail {
        let text = tail_lines(&content, n);
        print!("{text}");
        if !text.ends_with('\n') && !text.is_empty() {
            println!();
        }
    } else {
        print!("{content}");
        if !content.ends_with('\n') && !content.is_empty() {
            println!();
        }
    }
    Ok(())
}

/// Streams new bytes from `path` until the job exit footer is seen or interrupted.
async fn follow_log(path: &Path, tail: Option<usize>) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("opening log at {}", path.display()))?;
    let mut initial_content = String::new();
    file.read_to_string(&mut initial_content)?;

    if let Some(n) = tail {
        let text = tail_lines(&initial_content, n);
        print!("{text}");
        if !text.ends_with('\n') && !text.is_empty() {
            println!();
        }
    } else {
        print!("{initial_content}");
        if !initial_content.ends_with('\n') && !initial_content.is_empty() {
            println!();
        }
    }
    std::io::stdout().flush()?;

    if initial_content.contains("=== idlemax:") {
        return Ok(());
    }

    let mut buf = [0u8; 4096];
    loop {
        let n = file.read(&mut buf)?;
        if n > 0 {
            let chunk = &buf[..n];
            std::io::stdout().write_all(chunk)?;
            std::io::stdout().flush()?;
            let text = String::from_utf8_lossy(chunk);
            if text.contains("=== idlemax:") {
                break;
            }
        } else {
            let meta = std::fs::metadata(path)?;
            let current_pos = file.stream_position()?;
            if meta.len() < current_pos {
                file.seek(SeekFrom::Start(0))?;
                continue;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    Ok(())
}

/// When the local log file is absent, asks the coordinator which device ran the job.
async fn handle_missing_log(client: &ApiClient, path: &Path, id: JobId) -> Result<()> {
    match client.job(id).await {
        Ok(job) => {
            if let Some(device) = job.device {
                println!("job {id} ran on {device}; run this there");
            } else {
                println!("job {id} has not run yet (status: {})", job.status);
            }
            Ok(())
        }
        Err(ClientError::Api { status: 404, .. }) => {
            println!("job {id} not found");
            Ok(())
        }
        Err(e) => {
            bail!(
                "log file {} not found and coordinator unreachable: {}",
                path.display(),
                e
            );
        }
    }
}
