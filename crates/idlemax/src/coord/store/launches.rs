//! Worker launches (adapter): a Run and its first job, a relaunch of a Run, and a stop that
//! moves Runs and ends their jobs, each in one transaction, so a Run never points at a job
//! that does not exist and a job never runs for a Run that was stopped. Also the reads the
//! launch step needs: the weekly readings of an account and the last launch on a device.

use idlemax_core::types::{Job, JobSpec, JobStatus, QuotaSnapshot, QuotaWindowKind, UnixMs};
use idlemax_core::worker::run::{Run, RunLaunch, RunState};
use idlemax_core::worker::RunId;
use rusqlite::{params, OptionalExtension};

use super::jobs::{get_job, update_jobs, Added};
use super::quota::snapshot_from_row;
use super::runs::{insert_leased, insert_run, update_run};
use super::{json_col, json_text, Store, StoreError, StoreResult};

impl Store {
    /// A first launch: stores `run`, then its job made by `job_for` from the new Run id and
    /// leased to `device`, and points the Run at it.
    pub fn launch_run(
        &mut self,
        run: &Run,
        launch: &RunLaunch,
        job_for: impl FnOnce(RunId) -> JobSpec,
        expires_ms: UnixMs,
        now: UnixMs,
    ) -> StoreResult<(Run, Job)> {
        let live_jobs = self.caps.live_jobs;
        let tx = self.conn.transaction()?;
        let id = insert_run(&tx, run, launch)?;
        let spec = job_for(id);
        let job = new_job(insert_leased(
            &tx,
            live_jobs,
            &spec,
            &run.device,
            expires_ms,
            now,
        )?)?;
        let stored = Run {
            id,
            job: Some(job.id),
            launches: 1,
            ..run.clone()
        };
        update_run(&tx, &stored, run.state)?;
        tx.commit()?;
        Ok((stored, job))
    }

    /// A relaunch of a Run that is still in `expect`: its next job and the Run moved to
    /// `starting`. None, writing nothing, when the Run moved meanwhile.
    pub fn relaunch_run(
        &mut self,
        run: &Run,
        expect: RunState,
        spec: &JobSpec,
        expires_ms: UnixMs,
        now: UnixMs,
    ) -> StoreResult<Option<(Run, Job)>> {
        let live_jobs = self.caps.live_jobs;
        let tx = self.conn.transaction()?;
        let job = new_job(insert_leased(
            &tx,
            live_jobs,
            spec,
            &run.device,
            expires_ms,
            now,
        )?)?;
        let next = Run {
            state: RunState::Starting,
            job: Some(job.id),
            launches: run.launches + 1,
            end_ms: None,
            updated_ms: now,
            ..run.clone()
        };
        if !update_run(&tx, &next, expect)? {
            return Ok(None);
        }
        tx.commit()?;
        Ok(Some((next, job)))
    }

    /// The owner's keep or discard of a Run: its job leased to the Run's device and the Run
    /// pointed at it, while the Run is still in its state. None, writing nothing, when it moved.
    pub fn start_review(
        &mut self,
        run: &Run,
        spec: &JobSpec,
        expires_ms: UnixMs,
        now: UnixMs,
    ) -> StoreResult<Option<Job>> {
        let live_jobs = self.caps.live_jobs;
        let tx = self.conn.transaction()?;
        let job = new_job(insert_leased(
            &tx,
            live_jobs,
            spec,
            &run.device,
            expires_ms,
            now,
        )?)?;
        let next = Run {
            job: Some(job.id),
            updated_ms: now,
            ..run.clone()
        };
        if !update_run(&tx, &next, run.state)? {
            return Ok(None);
        }
        tx.commit()?;
        Ok(Some(job))
    }

    /// Moves each Run that is still in its expected state, and ends its live job `killed`
    /// with `why`, in one transaction. The agents hear `kill_job` on their next heartbeat.
    /// Returns the Runs that moved.
    pub fn stop_runs(
        &mut self,
        moved: &[(Run, RunState)],
        why: &str,
        now: UnixMs,
    ) -> StoreResult<Vec<Run>> {
        let tx = self.conn.transaction()?;
        let done = stop_in(&tx, moved, why, now)?;
        tx.commit()?;
        Ok(done)
    }

    /// The owner's stop (D11): the stop row, the Runs `stopped` and their jobs `killed`, in
    /// one transaction, so it survives a reboot and nothing launches while it is stored.
    pub fn owner_stop(
        &mut self,
        moved: &[(Run, RunState)],
        reason: &str,
        now: UnixMs,
    ) -> StoreResult<Vec<Run>> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO worker_stop (one, stopped_ms, reason) VALUES (1, ?1, ?2)
             ON CONFLICT (one) DO UPDATE SET stopped_ms = excluded.stopped_ms,
               reason = excluded.reason",
            params![now, reason],
        )?;
        let done = stop_in(&tx, moved, reason, now)?;
        tx.commit()?;
        Ok(done)
    }

    /// The owner's start: the stop row goes and the stopped Runs go back in the queue.
    pub fn owner_start(&mut self, moved: &[(Run, RunState)]) -> StoreResult<Vec<Run>> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM worker_stop", [])?;
        let mut done = Vec::new();
        for (run, expect) in moved {
            if update_run(&tx, run, *expect)? {
                done.push(run.clone());
            }
        }
        tx.commit()?;
        Ok(done)
    }

    /// What a relaunch of the Run needs; never sent out by a route.
    pub fn run_launch(&self, id: RunId) -> StoreResult<Option<RunLaunch>> {
        Ok(self
            .conn
            .query_row("SELECT launch FROM runs WHERE id = ?1", [id], |r| {
                json_col(r, 0)
            })
            .optional()?)
    }

    /// The weekly readings of `account` taken at or after `since_ms`, oldest first.
    pub fn weekly_readings(
        &self,
        account: &str,
        since_ms: UnixMs,
    ) -> StoreResult<Vec<QuotaSnapshot>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT account, quota_window, used_pct, resets_at_ms, source, at_ms
             FROM quota_snapshots WHERE account = ?1 AND quota_window = ?2 AND at_ms >= ?3
             ORDER BY at_ms",
        )?;
        let weekly = json_text(&QuotaWindowKind::Weekly)?;
        let rows = stmt.query_map(params![account, weekly, since_ms], snapshot_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The owner's stop of every worker, while it is in force: when, and why.
    pub fn worker_stop(&self) -> StoreResult<Option<(UnixMs, String)>> {
        Ok(self
            .conn
            .query_row("SELECT stopped_ms, reason FROM worker_stop", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?)
    }

    /// When the newest worker job of `device` was created.
    pub fn last_worker_launch(&self, device: &str) -> StoreResult<Option<UnixMs>> {
        Ok(self.conn.query_row(
            "SELECT max(created_ms) FROM jobs WHERE worker = 1 AND device = ?1",
            [device],
            |r| r.get(0),
        )?)
    }
}

/// The writes behind `stop_runs`; the caller owns the transaction.
fn stop_in(
    tx: &rusqlite::Connection,
    moved: &[(Run, RunState)],
    why: &str,
    now: UnixMs,
) -> StoreResult<Vec<Run>> {
    let mut done = Vec::new();
    for (run, expect) in moved {
        if !update_run(tx, run, *expect)? {
            continue;
        }
        let live = match run.job {
            Some(id) => get_job(tx, id)?.filter(|j| !j.status.is_terminal()),
            None => None,
        };
        if let Some(mut job) = live {
            job.status = JobStatus::Killed;
            job.finished_ms = Some(now);
            job.lease_expires_ms = None;
            job.last_error = Some(why.to_string());
            update_jobs(tx, std::slice::from_ref(&job))?;
        }
        done.push(run.clone());
    }
    Ok(done)
}

/// A worker job's key is its Run's, so a live one means the Run already has a job.
fn new_job(added: Added) -> StoreResult<Job> {
    match added {
        Added::New(job) => Ok(job),
        Added::Existing(job) => Err(StoreError::Invalid(format!(
            "job {} of this Run is still live",
            job.id
        ))),
    }
}
