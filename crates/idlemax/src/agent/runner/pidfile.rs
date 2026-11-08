//! Adapter: per-job files under `agent/jobs/` that let a restarted agent finish what a dead one
//! left behind. `<id>.pid` records who ran the job and enough of the job to report it;
//! `<id>.exit` is the exit record the job supervisor writes when the job ends.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use idlemax_core::job_run::{
    orphan_verdict, same_process, ExitMarker, JobKey, OrphanVerdict, RunClock, StopReason,
};
use idlemax_core::protocol::JobUpdate;
use idlemax_core::types::{DeviceId, Job, JobId, JobStatus, UnixMs};
use idlemax_core::Paths;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::finish::{self, Ended};
use super::os::{self, ProcTable, Sig};

/// What the agent keeps about a running job: enough to report its exit on its own.
#[derive(Debug, Serialize, Deserialize)]
pub struct Saved {
    pub job: Job,
    /// The attempt tag in the env of the job's processes; 0 in a file an older agent wrote.
    #[serde(default)]
    pub tag: u64,
    pub log_offset: u64,
    pub wrapper: String,
    pub memory_cap_mib: Option<u64>,
}

struct Record {
    id: JobId,
    pid: u32,
    started_ms: UnixMs,
    /// Absent in a file an older agent wrote: such a job can be killed but not reported.
    saved: Option<Saved>,
}

fn dir(paths: &Paths) -> PathBuf {
    paths.agent_state_dir().join("jobs")
}

fn file(paths: &Paths, id: JobId) -> PathBuf {
    dir(paths).join(format!("{id}.pid"))
}

/// Where the job supervisor records how the job ended.
pub fn marker_path(paths: &Paths, id: JobId) -> PathBuf {
    dir(paths).join(format!("{id}.exit"))
}

/// Records `<pid> <started_ms>` and the job for a running job. Written whole or not at all.
pub fn write(
    paths: &Paths,
    id: JobId,
    pid: u32,
    started_ms: UnixMs,
    mut saved: Saved,
) -> io::Result<()> {
    // The pid file is not the place for a private prompt or a secret env value.
    saved.job.spec.stdin = None;
    saved.job.spec.env.clear();
    let json = serde_json::to_string(&saved).map_err(io::Error::other)?;
    crate::state::create_private_dir(&dir(paths))?;
    let (path, tmp) = (file(paths, id), file(paths, id).with_extension("pid.tmp"));
    let mut out = crate::state::private_file()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    out.write_all(format!("{pid} {started_ms}\n{json}\n").as_bytes())?;
    std::fs::rename(tmp, path)
}

/// Forgets a job's files. A missing file is fine.
pub fn remove(paths: &Paths, id: JobId) {
    for path in [file(paths, id), marker_path(paths, id)] {
        remove_file(&path);
    }
}

/// Drops an exit record left by an earlier attempt, before a new attempt starts.
pub fn clear_marker(paths: &Paths, id: JobId) {
    remove_file(&marker_path(paths, id));
}

fn remove_file(path: &Path) {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            warn!(path = %path.display(), error = %e, "cannot remove job file");
        }
        _ => {}
    }
}

/// Settles jobs left by a previous agent process and returns their updates: kills their
/// group and members that left it, and reports the real exit of a job that ended while no
/// agent ran. Fails closed: the group is killed only when the root's OS start time still
/// matches the record; members found by env tag are killed either way.
///
/// A job that yields an update keeps its files. The caller removes them with `remove` once
/// the update is safely on its way, so a second crash repeats the report instead of losing it.
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
    for record in entries.filter_map(|e| parse(&e.ok()?.path())) {
        match reap_one(paths, procs, device, &record, now_ms) {
            Some(update) => updates.push(update),
            None => remove(paths, record.id),
        }
    }
    updates
}

fn reap_one(
    paths: &Paths,
    procs: &mut ProcTable,
    device: &DeviceId,
    record: &Record,
    now_ms: UnixMs,
) -> Option<JobUpdate> {
    let Record {
        id,
        pid,
        started_ms,
        ..
    } = *record;
    // The recorded root is ours to kill only while its start time still matches.
    let root_same = procs
        .start_ms(pid)
        .is_some_and(|os_start| same_process(started_ms, os_start));
    let job = JobKey {
        id,
        root: pid,
        started_ms,
        tag: record.saved.as_ref().map_or(0, |s| s.tag),
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
    let marker = std::fs::read_to_string(marker_path(paths, id))
        .ok()
        .and_then(|text| ExitMarker::parse(&text));
    let verdict = orphan_verdict(pid, root_same, marker);
    if root_same || !left.is_empty() {
        info!(
            job = id,
            pid,
            outside = left.len(),
            "killed orphaned job from a previous agent run"
        );
    }
    match verdict {
        OrphanVerdict::Exited(marker) => recovered(paths, device, record, &marker)
            .or_else(|| root_same.then(|| orphaned(device, record, now_ms))),
        // A job whose root is already gone is settled by its lease; only a live root is reported.
        OrphanVerdict::Killed => Some(orphaned(device, record, now_ms)),
        OrphanVerdict::Gone => None,
    }
}

/// The job ended while no agent ran: report the exit its supervisor recorded, through the same
/// path as a normal finish (log footer, inbox note, update).
fn recovered(
    paths: &Paths,
    device: &DeviceId,
    record: &Record,
    marker: &ExitMarker,
) -> Option<JobUpdate> {
    let saved = record.saved.as_ref()?;
    let ended = Ended {
        job: saved.job.clone(),
        exit: marker.exit(),
        stop: None,
        clock: RunClock::start(record.started_ms, 0),
        active_ms: (marker.ended_ms - record.started_ms).max(0),
        log_path: paths.job_log_file(record.id),
        log_offset: saved.log_offset,
        memory_cap_mib: saved.memory_cap_mib,
    };
    let update = finish::finish(paths, device, &saved.wrapper, ended, marker.ended_ms);
    info!(job = record.id, status = %update.status, "reporting a job that ended while the agent was down");
    Some(update)
}

fn orphaned(device: &DeviceId, record: &Record, now_ms: UnixMs) -> JobUpdate {
    JobUpdate {
        id: record.id,
        device: device.clone(),
        attempt: record.saved.as_ref().map_or(0, |s| s.job.attempts),
        status: JobStatus::Killed,
        at_ms: now_ms,
        error: StopReason::Orphaned.message(),
        // The agent restarted under the job; that is not the job's fault.
        retryable: true,
        ..JobUpdate::default()
    }
}

fn parse(path: &Path) -> Option<Record> {
    let id = path
        .file_name()?
        .to_str()?
        .strip_suffix(".pid")?
        .parse()
        .ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let mut head = lines.next()?.split_whitespace();
    let pid = head.next()?.parse().ok()?;
    let started_ms = head.next()?.parse().ok()?;
    let saved = lines
        .next()
        .and_then(|json| serde_json::from_str(json).ok());
    Some(Record {
        id,
        pid,
        started_ms,
        saved,
    })
}
