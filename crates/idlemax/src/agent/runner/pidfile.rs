//! Adapter: pid files under `agent/jobs/` so a restarted agent can kill orphaned jobs.

use std::io;
use std::path::{Path, PathBuf};

use idlemax_core::job_run::{same_process, JobKey, StopReason};
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

/// Kills jobs left by a previous agent process (their group, and members that left it)
/// and returns Killed updates. Fails closed: the group is killed only when the root's OS
/// start time matches the record; members found by env tag are killed either way.
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
    // The recorded root is ours to kill only while its start time still matches.
    let root_same = procs
        .start_ms(pid)
        .is_some_and(|os_start| same_process(started_ms, os_start));
    let job = JobKey {
        id,
        root: pid,
        started_ms,
    };
    let left = procs.orphans(&job, root_same).outside_group;
    let killed = if root_same {
        os::signal_job(pid, &left, Sig::Kill)
    } else {
        os::signal_pids(&left, Sig::Kill);
        Ok(())
    };
    if let Err(e) = killed {
        warn!(job = id, pid, error = %e, "cannot kill orphaned job");
        return None;
    }
    if root_same || !left.is_empty() {
        info!(
            job = id,
            pid,
            outside = left.len(),
            "killed orphaned job from a previous agent run"
        );
    }
    // A job whose root is already gone is settled by its lease; only a live root is reported.
    root_same.then(|| JobUpdate {
        id,
        device: device.clone(),
        status: JobStatus::Killed,
        at_ms: now_ms,
        error: StopReason::Orphaned.message(),
        // The agent restarted under the job; that is not the job's fault.
        retryable: true,
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
