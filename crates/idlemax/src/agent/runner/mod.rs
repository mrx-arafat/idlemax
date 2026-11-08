//! Adapter: runs leased jobs as OS processes under throttles, enforces caps, writes job
//! logs and inbox notes. Decisions live in `idlemax_core::job_run`. Sync and
//! `Send + 'static`: the agent drives it from `spawn_blocking` and calls `poll` every few seconds.

#[cfg(all(test, unix))]
mod daemon_tests;
mod finish;
mod launch;
mod os;
mod pidfile;
#[cfg(all(test, unix))]
mod tests;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::{Child, ExitStatus};
use std::time::Instant;

use idlemax_core::config::AgentConfig;
use idlemax_core::job_run::{self, AdmitError, ExitInfo, JobKey, RunClock, StopReason, Wrapper};
use idlemax_core::protocol::{JobUpdate, RunningJob};
use idlemax_core::types::{DeviceId, Job, JobId, JobStatus, UnixMs};
use idlemax_core::Paths;
use tracing::{info, warn};

use crate::state::now_ms;
use finish::Ended;
use os::{ProcTable, Sig, Usage};

/// Minimum gap between process table refreshes, however often `poll` runs.
const USAGE_REFRESH_MS: i64 = 1_000;

// The agent moves the runner into `spawn_blocking`.
const _: fn() = || {
    fn send_static<T: Send + 'static>() {}
    send_static::<Runner>();
};

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("job {0} is already running")]
    AlreadyRunning(JobId),
    #[error("job refused: {0}")]
    Refused(#[from] AdmitError),
    #[error("command is empty")]
    EmptyCommand,
    #[error("job stdin pipe unavailable")]
    StdinUnavailable,
    #[error("write job stdin: {0}")]
    Stdin(#[source] io::Error),
    #[error("program {0:?} not found")]
    ProgramNotFound(String),
    #[error("program {0:?} must be a bare name or an absolute path")]
    RelativeProgram(String),
    #[error("cwd {} is not a directory", .0.display())]
    BadCwd(PathBuf),
    #[error("no home directory for the default cwd")]
    NoHome,
    #[error("{what} {}: {source}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("signal job {id}: {source}")]
    Signal {
        id: JobId,
        #[source]
        source: io::Error,
    },
}

pub struct Runner {
    paths: Paths,
    device: DeviceId,
    allowlist: Vec<String>,
    wrapper: Wrapper,
    procs: Option<ProcTable>,
    usage_at_ms: UnixMs,
    slots: HashMap<JobId, Slot>,
    pending: Vec<JobUpdate>,
    awake_origin: Instant,
}

struct Slot {
    job: Job,
    child: Child,
    pid: u32,
    clock: RunClock,
    log_path: PathBuf,
    log_offset: u64,
    usage: Option<Usage>,
    /// Members outside the job's process group, signalled by pid.
    outside: Vec<u32>,
    stop: Option<Stop>,
}

struct Stop {
    reason: StopReason,
    term_sent_ms: UnixMs,
    kill_sent: bool,
}

impl Runner {
    /// Detects the throttle and kills jobs orphaned by a previous agent process;
    /// their Killed updates come out of the first `poll`.
    pub fn new(paths: Paths, device: DeviceId, config: AgentConfig) -> Self {
        let wrapper = os::detect_wrapper();
        let mut procs = ProcTable::new();
        if procs.is_none() {
            warn!("process table unavailable: memory cap left to the OS throttle");
        }
        let pending = pidfile::reap_orphans(&paths, &device, procs.as_mut(), now_ms());
        info!(wrapper = wrapper.name(), "job runner ready");
        Self {
            paths,
            device,
            allowlist: config.command_allowlist,
            wrapper,
            procs,
            usage_at_ms: 0,
            slots: HashMap::new(),
            pending,
            awake_origin: Instant::now(),
        }
    }

    /// Starts a leased job. On error nothing runs and no update is queued.
    pub fn start(&mut self, job: &Job) -> Result<(), RunnerError> {
        let started_ms = now_ms();
        let awake_ms = self.awake_ms();
        self.start_at(job, started_ms, awake_ms)
    }

    fn start_at(
        &mut self,
        job: &Job,
        started_ms: UnixMs,
        awake_ms: i64,
    ) -> Result<(), RunnerError> {
        if self.slots.contains_key(&job.id) {
            return Err(RunnerError::AlreadyRunning(job.id));
        }
        job_run::admit(&job.spec, &self.allowlist)?;
        let launched = launch::launch(&self.paths, &self.wrapper, job, started_ms)?;
        if let Err(e) = pidfile::write(&self.paths, job.id, launched.pid, started_ms) {
            warn!(job = job.id, error = %e, "cannot write job pid file");
        }
        info!(job = job.id, pid = launched.pid, "job started");
        let slot = Slot {
            job: job.clone(),
            child: launched.child,
            pid: launched.pid,
            clock: RunClock::start(started_ms, awake_ms),
            log_path: launched.log_path,
            log_offset: launched.log_offset,
            usage: None,
            outside: Vec::new(),
            stop: None,
        };
        self.slots.insert(job.id, slot);
        Ok(())
    }

    /// SIGSTOP the job's process group. Unknown, stopping or paused jobs: no-op.
    pub fn pause(&mut self, id: JobId) -> Result<(), RunnerError> {
        let awake_ms = self.awake_ms();
        let Some(slot) = self.slots.get_mut(&id) else {
            return Ok(());
        };
        if slot.stop.is_some() || slot.clock.is_paused() {
            return Ok(());
        }
        os::signal_job(slot.pid, &slot.outside, Sig::Stop)
            .map_err(|source| RunnerError::Signal { id, source })?;
        slot.clock.pause(awake_ms);
        Ok(())
    }

    /// SIGCONT the job's process group. Unknown or running jobs: no-op.
    pub fn resume(&mut self, id: JobId) -> Result<(), RunnerError> {
        let awake_ms = self.awake_ms();
        let Some(slot) = self.slots.get_mut(&id) else {
            return Ok(());
        };
        if !slot.clock.is_paused() {
            return Ok(());
        }
        os::signal_job(slot.pid, &slot.outside, Sig::Cont)
            .map_err(|source| RunnerError::Signal { id, source })?;
        slot.clock.resume(awake_ms);
        Ok(())
    }

    /// SIGTERM the group (SIGCONT too if paused); `poll` sends SIGKILL after 15 s.
    /// The Killed update carries `error: None` so the caller can add its reason.
    pub fn kill(&mut self, id: JobId) -> Result<(), RunnerError> {
        let awake_ms = self.awake_ms();
        match self.slots.get_mut(&id) {
            Some(slot) => stop(slot, StopReason::Requested, now_ms(), awake_ms)
                .map_err(|source| RunnerError::Signal { id, source }),
            None => Ok(()),
        }
    }

    /// Enforces max runtime and the memory cap, escalates kills, reaps exits.
    /// Returns terminal updates only (succeeded, failed, killed).
    pub fn poll(&mut self) -> Vec<JobUpdate> {
        let now = now_ms();
        self.poll_at(now, self.awake_ms())
    }

    fn poll_at(&mut self, now: UnixMs, awake_ms: i64) -> Vec<JobUpdate> {
        self.refresh_usage(now);
        for slot in self.slots.values_mut() {
            enforce(slot, now, awake_ms);
        }
        let mut updates = std::mem::take(&mut self.pending);
        updates.extend(self.reap(now, awake_ms));
        updates
    }

    // Rust's Instant uses CLOCK_UPTIME_RAW on macOS and CLOCK_MONOTONIC on Linux.
    // Both exclude system suspend; Unix time remains for reports and pid identity.
    fn awake_ms(&self) -> i64 {
        i64::try_from(self.awake_origin.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    pub fn running(&self) -> Vec<RunningJob> {
        let mut jobs: Vec<RunningJob> = self.slots.values().map(Slot::view).collect();
        jobs.sort_by_key(|j| j.id);
        jobs
    }

    fn refresh_usage(&mut self, now: UnixMs) {
        if self.slots.is_empty() || now - self.usage_at_ms < USAGE_REFRESH_MS {
            return;
        }
        let Some(procs) = self.procs.as_mut() else {
            return;
        };
        procs.refresh();
        self.usage_at_ms = now;
        let jobs: Vec<JobKey> = self.slots.values().map(Slot::key).collect();
        let mut found = procs.usages(&jobs);
        for slot in self.slots.values_mut() {
            slot.usage = None;
            if let Some(mine) = found.remove(&slot.pid) {
                slot.usage = Some(mine.usage);
                slot.outside = mine.outside_group;
            }
        }
    }

    fn reap(&mut self, now: UnixMs, awake_ms: i64) -> Vec<JobUpdate> {
        let ended: Vec<(JobId, ExitStatus)> = self
            .slots
            .iter_mut()
            .filter_map(|(id, slot)| match slot.child.try_wait() {
                Ok(status) => status.map(|s| (*id, s)),
                Err(e) => {
                    warn!(job = *id, error = %e, "cannot check job exit");
                    None
                }
            })
            .collect();
        ended
            .into_iter()
            .filter_map(|(id, status)| {
                self.slots
                    .remove(&id)
                    .map(|slot| self.close(slot, status, now, awake_ms))
            })
            .collect()
    }

    fn close(&self, slot: Slot, status: ExitStatus, now: UnixMs, awake_ms: i64) -> JobUpdate {
        if let Err(e) = os::kill_leftovers(slot.pid, &slot.outside) {
            warn!(job = slot.job.id, error = %e, "cannot kill leftover job processes");
        }
        pidfile::remove(&self.paths, slot.job.id);
        let memory_cap_mib = self
            .wrapper
            .enforces_memory_cap()
            .then_some(slot.job.spec.mem_mb);
        let ended = Ended {
            job: slot.job,
            exit: exit_info(status),
            stop: slot.stop.map(|s| s.reason),
            clock: slot.clock,
            active_ms: slot.clock.active_ms(awake_ms),
            log_path: slot.log_path,
            log_offset: slot.log_offset,
            memory_cap_mib,
        };
        let update = finish::finish(&self.paths, &self.device, self.wrapper.name(), ended, now);
        info!(job = update.id, status = %update.status, "job ended");
        update
    }
}

/// A dropped runner must not leave unmanaged jobs behind: SIGKILL every group.
impl Drop for Runner {
    fn drop(&mut self) {
        for (id, slot) in &self.slots {
            match os::signal_job(slot.pid, &slot.outside, Sig::Kill) {
                Ok(()) => pidfile::remove(&self.paths, *id),
                Err(e) => warn!(job = *id, error = %e, "cannot kill job on runner drop"),
            }
        }
    }
}

impl Slot {
    fn key(&self) -> JobKey {
        JobKey {
            id: self.job.id,
            root: self.pid,
            started_ms: self.clock.started_ms(),
        }
    }

    fn view(&self) -> RunningJob {
        let spec = &self.job.spec;
        let status = if self.clock.is_paused() {
            JobStatus::Paused
        } else {
            JobStatus::Running
        };
        RunningJob {
            id: self.job.id,
            name: spec.name.clone(),
            status,
            pid: Some(self.pid),
            started_ms: self.clock.started_ms(),
            mem_mb: spec.mem_mb,
            rss_bytes: self.usage.map(|u| u.rss_bytes),
            cpu_pct: self.usage.map(|u| u.cpu_pct),
            cost: spec.cost,
            ai_account: spec.ai_account.clone(),
        }
    }
}

fn enforce(slot: &mut Slot, now: UnixMs, awake_ms: i64) {
    let id = slot.job.id;
    if let Some(stop) = slot.stop.as_mut() {
        if !stop.kill_sent && job_run::kill_due(stop.term_sent_ms, now) {
            match os::signal_job(slot.pid, &slot.outside, Sig::Kill) {
                Ok(()) => stop.kill_sent = true,
                Err(e) => warn!(job = id, error = %e, "cannot SIGKILL job"),
            }
        }
        return;
    }
    let rss = slot.usage.map(|u| u.rss_bytes);
    if let Some(reason) = job_run::breach(&slot.job.spec, slot.clock.active_ms(awake_ms), rss) {
        warn!(job = id, reason = ?reason.message(), "stopping job over its cap");
        if let Err(e) = stop(slot, reason, now, awake_ms) {
            warn!(job = id, error = %e, "cannot stop job");
        }
    }
}

fn stop(slot: &mut Slot, reason: StopReason, now: UnixMs, awake_ms: i64) -> io::Result<()> {
    if slot.stop.is_some() {
        return Ok(());
    }
    os::signal_job(slot.pid, &slot.outside, Sig::Term)?;
    slot.stop = Some(Stop {
        reason,
        term_sent_ms: now,
        kill_sent: false,
    });
    if slot.clock.is_paused() {
        // A stopped process acts on SIGTERM only once continued.
        os::signal_job(slot.pid, &slot.outside, Sig::Cont)?;
        slot.clock.resume(awake_ms);
    }
    Ok(())
}

fn exit_info(status: ExitStatus) -> ExitInfo {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal = None;
    ExitInfo {
        code: status.code(),
        signal,
    }
}
