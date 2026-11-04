//! Adapter: pid files under `agent/jobs/` so a restarted agent can kill orphaned jobs.

use std::io;
use std::path::{Path, PathBuf};

use idlemax_core::job_run::{same_process, StopReason};
use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{DeviceId, JobId, JobStatus, UnixMs};
use idlemax_core::Paths;
use tracing::{info, warn};

use super::os::{self, ProcTable, Sig};

fn dir(paths: &Paths) -> PathBuf {
    paths.agent_state_dir().join("jobs")
}

fn file(paths: &Paths, id: JobId) -> PathBuf {
    dir(paths).join(format!("{id}.pid"))
}

/// Records `<pid> <started_ms>` for a running job.
pub fn write(paths: &Paths, id: JobId, pid: u32, started_ms: UnixMs) -> io::Result<()> {
    std::fs::create_dir_all(dir(paths))?;
    std::fs::write(file(paths, id), format!("{pid} {started_ms}\n"))
}

pub fn remove(paths: &Paths, id: JobId) {
    match std::fs::remove_file(file(paths, id)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            warn!(job = id, error = %e, "cannot remove job pid file");
        }
        _ => {}
    }
}

/// Kills job groups left by a previous agent process and returns Killed updates.
/// Fails closed: a pid is killed only when its OS start time matches the record.
pub fn reap_orphans(
    paths: &Paths,
    device: &DeviceId,
    procs: Option<&mut ProcTable>,
    now_ms: UnixMs,
) -> Vec<JobUpdate> {
    let entries = match std::fs::read_dir(dir(paths)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!(error = %e, "cannot list job pid files");
            return Vec::new();
        }
    };
    let Some(procs) = procs else {
        warn!("process table unavailable; leaving job pid files for the next start");
        return Vec::new();
    };
    let mut updates = Vec::new();
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        if let Some((id, pid, started_ms)) = parse(&path) {
            if let Some(update) = reap_one(procs, device, id, pid, started_ms, now_ms) {
                updates.push(update);
            }
            remove(paths, id);
        }
    }
    updates
}

fn reap_one(
    procs: &mut ProcTable,
    device: &DeviceId,
    id: JobId,
    pid: u32,
    started_ms: UnixMs,
    now_ms: UnixMs,
) -> Option<JobUpdate> {
    let os_start = procs.start_ms(pid)?;
    if !same_process(started_ms, os_start) {
        return None;
    }
    if let Err(e) = os::signal_group(pid, Sig::Kill) {
        warn!(job = id, pid, error = %e, "cannot kill orphaned job");
        return None;
    }
    info!(
        job = id,
        pid, "killed orphaned job from a previous agent run"
    );
    Some(JobUpdate {
        id,
        device: device.clone(),
        status: JobStatus::Killed,
        at_ms: now_ms,
        error: StopReason::Orphaned.message(),
        ..JobUpdate::default()
    })
}

fn parse(path: &Path) -> Option<(JobId, u32, UnixMs)> {
    let id = path
        .file_name()?
        .to_str()?
        .strip_suffix(".pid")?
        .parse()
        .ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    let started_ms = parts.next()?.parse().ok()?;
    Some((id, pid, started_ms))
}
