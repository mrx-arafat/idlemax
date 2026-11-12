//! Run rows (adapter): the record of each worker on one task, and worker jobs that are
//! created already leased to one device. The domain decides every state move; `update_run`
//! only refuses to overwrite a state that moved in between.

use idlemax_core::types::{Job, JobSpec, UnixMs};
use idlemax_core::worker::run::{after_owner_stop, Run, RunLaunch, RunState};
use idlemax_core::worker::RunId;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::jobs::{check_live_cap, get_job, unfinished_with_key, Added};
use super::{enum_col, enum_text, json_col, json_text, sql_limit, Store, StoreError, StoreResult};

const COLUMNS: &str = "id, device, project, task, task_title, source, branch, worktree, \
    provider, account, model, session, state, verdict, concern, reason, summary, facts, \
    quota_before, quota_after, job, launches, start_tries, start_ms, end_ms, updated_ms";

impl Store {
    pub fn get_run(&self, id: RunId) -> StoreResult<Option<Run>> {
        let sql = format!("SELECT {COLUMNS} FROM runs WHERE id = ?1");
        Ok(self.conn.query_row(&sql, [id], run_from_row).optional()?)
    }

    /// Newest first; `limit` 0 means all.
    pub fn list_runs(
        &self,
        state: Option<RunState>,
        project: Option<&str>,
        limit: u32,
    ) -> StoreResult<Vec<Run>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM runs
             WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR project = ?2)
             ORDER BY id DESC LIMIT ?3"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            params![state.map(RunState::as_str), project, sql_limit(limit)],
            run_from_row,
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

// Tests build Runs and leased jobs directly; the launch step writes them together through
// `launch_run` and `relaunch_run`.
#[cfg(test)]
impl Store {
    /// Stores a new Run with what its relaunches need and returns it with its id.
    pub fn insert_run(&mut self, run: &Run, launch: &RunLaunch) -> StoreResult<Run> {
        let id = insert_run(&self.conn, run, launch)?;
        self.get_run(id)?
            .ok_or_else(|| StoreError::Corrupt(format!("run {id}")))
    }

    /// Writes `run` only while its stored state is still `expect`; false when it moved, so
    /// a late agent update never overwrites the owner's stop.
    pub fn update_run(&mut self, run: &Run, expect: RunState) -> StoreResult<bool> {
        update_run(&self.conn, run, expect)
    }

    /// Inserts a worker job already leased to `device` with one attempt, so the scheduler's
    /// `pick` never sees it. A key that a live job holds returns that job and inserts nothing.
    pub fn insert_leased_job(
        &mut self,
        spec: &JobSpec,
        device: &str,
        expires_ms: UnixMs,
        now: UnixMs,
    ) -> StoreResult<Added> {
        let live_jobs = self.caps.live_jobs;
        let tx = self.conn.transaction()?;
        let added = insert_leased(&tx, live_jobs, spec, device, expires_ms, now)?;
        tx.commit()?;
        Ok(added)
    }
}

impl Store {
    /// Job and Run changes in one transaction, so a Run never misses its job's end.
    pub fn update_jobs_and_runs(
        &mut self,
        jobs: &[Job],
        runs: &[(Run, RunState)],
    ) -> StoreResult<()> {
        let tx = self.conn.transaction()?;
        write_jobs_and_runs(&tx, jobs, runs)?;
        tx.commit()?;
        Ok(())
    }
}

/// The write behind `insert_run`: the new Run's id. The caller owns the transaction.
pub(super) fn insert_run(conn: &Connection, run: &Run, launch: &RunLaunch) -> StoreResult<RunId> {
    conn.execute(
        "INSERT INTO runs (device, project, task, task_title, source, branch, worktree,
               provider, account, model, session, state, verdict, concern, reason, summary,
               facts, quota_before, quota_after, job, launches, start_tries, start_ms, end_ms,
               updated_ms, launch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
               ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
        params![
            run.device,
            run.project,
            run.task,
            run.task_title,
            run.source,
            run.branch,
            run.worktree,
            enum_text(&run.provider)?,
            run.account,
            run.model,
            run.session,
            run.state.as_str(),
            run.verdict.map(|v| v.as_str()),
            run.concern,
            run.reason,
            run.summary,
            json_text(&run.facts)?,
            json_text(&run.quota_before)?,
            json_text(&run.quota_after)?,
            run.job,
            run.launches,
            run.start_tries,
            run.start_ms,
            run.end_ms,
            run.updated_ms,
            json_text(launch)?,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The write behind `insert_leased_job`; the caller owns the transaction.
pub(super) fn insert_leased(
    tx: &Connection,
    live_jobs: i64,
    spec: &JobSpec,
    device: &str,
    expires_ms: UnixMs,
    now: UnixMs,
) -> StoreResult<Added> {
    spec.validate().map_err(StoreError::InvalidSpec)?;
    if let Some(key) = &spec.key {
        if let Some(job) = unfinished_with_key(tx, key)? {
            return Ok(Added::Existing(job));
        }
    }
    check_live_cap(tx, live_jobs)?;
    tx.execute(
        "INSERT INTO jobs (spec, name, status, priority, ai_account, created_ms, key, device,
               lease_expires_ms, attempts, worker)
             VALUES (?1, ?2, 'leased', ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)",
        params![
            json_text(spec)?,
            spec.name,
            spec.priority,
            spec.ai_account,
            now,
            spec.key,
            device,
            expires_ms,
            spec.worker.is_some(),
        ],
    )?;
    let id = tx.last_insert_rowid();
    let job: Job = get_job(tx, id)?.ok_or(StoreError::JobNotFound(id))?;
    Ok(Added::New(job))
}

/// Moves to `stopped` the Run of each worker job a coordinator control (`stop_all`,
/// `cancel`) just ended, because the owner asked for it (D11). The caller owns the
/// transaction, so the jobs and their Runs commit together.
pub(super) fn stop_runs_of(conn: &Connection, ended: &[Job], now: UnixMs) -> StoreResult<()> {
    for job in ended.iter().filter(|j| j.status.is_terminal()) {
        let Some(worker) = &job.spec.worker else {
            continue;
        };
        let sql = format!("SELECT {COLUMNS} FROM runs WHERE id = ?1");
        let run = conn
            .query_row(&sql, [worker.run], run_from_row)
            .optional()?;
        if let Some((run, next)) = run.and_then(|r| after_owner_stop(&r, job, now).map(|n| (r, n)))
        {
            update_run(conn, &next, run.state)?;
        }
    }
    Ok(())
}

/// Writes job and Run changes together: the sweep's expired worker jobs and their Runs.
pub(super) fn write_jobs_and_runs(
    conn: &Connection,
    jobs: &[Job],
    runs: &[(Run, RunState)],
) -> StoreResult<()> {
    super::jobs::update_jobs(conn, jobs)?;
    for (run, expect) in runs {
        update_run(conn, run, *expect)?;
    }
    Ok(())
}

/// The write behind `update_run`; the caller owns the transaction. `launch` never changes.
pub(super) fn update_run(conn: &Connection, run: &Run, expect: RunState) -> StoreResult<bool> {
    let changed = conn.execute(
        "UPDATE runs SET branch = ?2, worktree = ?3, session = ?4, state = ?5, verdict = ?6,
           concern = ?7, summary = ?8, facts = ?9, quota_before = ?10, quota_after = ?11,
           job = ?12, launches = ?13, start_tries = ?14, end_ms = ?15, updated_ms = ?16,
           reason = ?17
         WHERE id = ?1 AND state = ?18",
        params![
            run.id,
            run.branch,
            run.worktree,
            run.session,
            run.state.as_str(),
            run.verdict.map(|v| v.as_str()),
            run.concern,
            run.summary,
            json_text(&run.facts)?,
            json_text(&run.quota_before)?,
            json_text(&run.quota_after)?,
            run.job,
            run.launches,
            run.start_tries,
            run.end_ms,
            run.updated_ms,
            run.reason,
            expect.as_str(),
        ],
    )?;
    Ok(changed == 1)
}

fn run_from_row(r: &Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        device: r.get(1)?,
        project: r.get(2)?,
        task: r.get(3)?,
        task_title: r.get(4)?,
        source: r.get(5)?,
        branch: r.get(6)?,
        worktree: r.get(7)?,
        provider: enum_col(r, 8)?,
        account: r.get(9)?,
        model: r.get(10)?,
        session: r.get(11)?,
        state: enum_col(r, 12)?,
        verdict: r
            .get::<_, Option<String>>(13)?
            .map(|v| serde_json::from_value(serde_json::Value::String(v)))
            .transpose()
            .map_err(|e| super::conversion_err(13, e))?,
        concern: r.get(14)?,
        reason: r.get(15)?,
        summary: r.get(16)?,
        facts: json_col(r, 17)?,
        quota_before: json_col(r, 18)?,
        quota_after: json_col(r, 19)?,
        job: r.get(20)?,
        launches: r.get(21)?,
        start_tries: r.get(22)?,
        start_ms: r.get(23)?,
        end_ms: r.get(24)?,
        updated_ms: r.get(25)?,
    })
}

#[cfg(test)]
mod tests;
