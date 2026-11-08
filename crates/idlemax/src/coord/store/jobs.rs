//! Job queue and leases (adapter). A lease is the device plus `lease_expires_ms`
//! of an active job; the schema allows one active AI job per account.

use idlemax_core::protocol::JobCounts;
use idlemax_core::types::{Job, JobId, JobSpec, JobStatus, UnixMs};
use rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE;
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{conversion_err, json_col, json_text, sql_limit, Store, StoreError, StoreResult};

const JOB_COLUMNS: &str = "id, spec, status, device, attempts, created_ms, started_ms, \
    finished_ms, lease_expires_ms, not_before_ms, exit_code, last_error, output_path";

const ACTIVE: &str = "('leased', 'running', 'paused')";
/// Statuses that are not terminal; a test pins this list to `JobStatus::is_terminal`.
pub(super) const LIVE: &str = "('queued', 'leased', 'running', 'paused')";

/// What `insert_job_once` did.
#[derive(Debug, Clone, PartialEq)]
pub enum Added {
    /// The spec was queued as a new job.
    New(Job),
    /// A job with the spec's key had not finished, so nothing was queued.
    Existing(Job),
}

impl Added {
    pub fn into_job(self) -> Job {
        match self {
            Added::New(job) | Added::Existing(job) => job,
        }
    }
}

impl Store {
    /// Queues a validated spec without looking at its key; production code uses
    /// `insert_job_once`.
    #[cfg(test)]
    pub fn insert_job(&mut self, spec: &JobSpec, now: UnixMs) -> StoreResult<Job> {
        insert_job(&self.conn, self.caps.live_jobs, spec, now)
    }

    /// Queues a validated spec unless its key already names an unfinished job, in which
    /// case that job is returned. Check and insert share one transaction.
    pub fn insert_job_once(&mut self, spec: &JobSpec, now: UnixMs) -> StoreResult<Added> {
        let live_jobs = self.caps.live_jobs;
        let tx = self.conn.transaction()?;
        if let Some(key) = &spec.key {
            if let Some(job) = unfinished_with_key(&tx, key)? {
                return Ok(Added::Existing(job));
            }
        }
        let job = insert_job(&tx, live_jobs, spec, now)?;
        tx.commit()?;
        Ok(Added::New(job))
    }

    pub fn get_job(&self, id: JobId) -> StoreResult<Option<Job>> {
        get_job(&self.conn, id)
    }

    /// Newest first; `limit` 0 means all.
    pub fn list_jobs(&self, status: Option<JobStatus>, limit: u32) -> StoreResult<Vec<Job>> {
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE ?1 IS NULL OR status = ?1 ORDER BY id DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(
            params![status.map(JobStatus::as_str), sql_limit(limit)],
            job_from_row,
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every Queued job, oldest first (the scheduler orders by priority itself).
    pub fn queued_jobs(&self) -> StoreResult<Vec<Job>> {
        self.jobs_where("status = 'queued'")
    }

    /// Leased, running and paused jobs on every device, oldest first.
    pub fn active_jobs(&self) -> StoreResult<Vec<Job>> {
        self.jobs_where(&format!("status IN {ACTIVE}"))
    }

    fn jobs_where(&self, filter: &str) -> StoreResult<Vec<Job>> {
        let sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE {filter} ORDER BY id");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], job_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Writes every field except `created_ms` as the domain decided it, all or nothing.
    /// Use `lease_job` to assign: this write does not check the job is still queued.
    pub fn update_jobs(&mut self, jobs: &[Job]) -> StoreResult<()> {
        let tx = self.conn.transaction()?;
        update_jobs(&tx, jobs)?;
        tx.commit()?;
        Ok(())
    }

    /// (account, started_ms) of AI jobs started at or after `since_ms`, oldest first.
    pub fn ai_job_starts(&self, since_ms: UnixMs) -> StoreResult<Vec<(String, UnixMs)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT ai_account, started_ms FROM jobs
             WHERE ai_account IS NOT NULL AND started_ms >= ?1 ORDER BY started_ms",
        )?;
        let rows = stmt.query_map([since_ms], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Atomically moves a due Queued job to Leased on `device`, counting one attempt.
    /// Ok(None) when the job is gone, no longer queued or still backing off;
    /// `AccountLeased` when its AI account already holds a lease.
    pub fn lease_job(
        &mut self,
        id: JobId,
        device: &str,
        expires_ms: UnixMs,
        now: UnixMs,
    ) -> StoreResult<Option<Job>> {
        let account: Option<String> = self
            .conn
            .query_row("SELECT ai_account FROM jobs WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let changed = self
            .conn
            .execute(
                "UPDATE jobs SET status = 'leased', device = ?2, lease_expires_ms = ?3,
                   attempts = attempts + 1, not_before_ms = NULL,
                   started_ms = NULL, finished_ms = NULL, exit_code = NULL, output_path = NULL
                 WHERE id = ?1 AND status = 'queued'
                   AND (not_before_ms IS NULL OR not_before_ms <= ?4)",
                params![id, device, expires_ms, now],
            )
            .map_err(|e| lease_conflict(e, account.as_deref()))?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_job(id)
    }

    /// AI accounts with an active lease anywhere in the cluster.
    pub fn ai_leased_accounts(&self) -> StoreResult<Vec<String>> {
        let sql = format!(
            "SELECT ai_account FROM jobs
             WHERE ai_account IS NOT NULL AND status IN {ACTIVE} ORDER BY ai_account"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn job_counts(&self) -> StoreResult<JobCounts> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT status, count(*) FROM jobs GROUP BY status")?;
        let rows = stmt.query_map([], |r| Ok((status_col(r, 0)?, r.get::<_, i64>(1)?)))?;
        let mut counts = JobCounts::default();
        for row in rows {
            let (status, n) = row?;
            counts.add(status, super::u64_from_sql(n));
        }
        Ok(counts)
    }
}

/// The writes behind `update_jobs`; the caller owns the transaction.
pub(super) fn update_jobs(conn: &Connection, jobs: &[Job]) -> StoreResult<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE jobs SET spec = ?2, name = ?3, status = ?4, priority = ?5,
           ai_account = ?6, device = ?7, attempts = ?8, started_ms = ?9,
           finished_ms = ?10, lease_expires_ms = ?11, not_before_ms = ?12,
           exit_code = ?13, last_error = ?14, output_path = ?15
         WHERE id = ?1",
    )?;
    for job in jobs {
        let changed = stmt
            .execute(params![
                job.id,
                json_text(&job.spec)?,
                job.spec.name,
                job.status.as_str(),
                job.spec.priority,
                job.spec.ai_account,
                job.device,
                job.attempts,
                job.started_ms,
                job.finished_ms,
                job.lease_expires_ms,
                job.not_before_ms,
                job.exit_code,
                job.last_error,
                job.output_path,
            ])
            .map_err(|e| lease_conflict(e, job.spec.ai_account.as_deref()))?;
        if changed == 0 {
            return Err(StoreError::JobNotFound(job.id));
        }
        if job.status.is_terminal() {
            conn.execute(
                "DELETE FROM controls WHERE scope_type = 'job' AND scope_id = ?1",
                [job.id.to_string()],
            )?;
        }
    }
    Ok(())
}

/// Maps the one-AI-lease-per-account index violation to `AccountLeased`.
fn lease_conflict(e: rusqlite::Error, account: Option<&str>) -> StoreError {
    match (&e, account) {
        (rusqlite::Error::SqliteFailure(f, _), Some(account))
            if f.extended_code == SQLITE_CONSTRAINT_UNIQUE =>
        {
            StoreError::AccountLeased(account.to_owned())
        }
        _ => StoreError::Sqlite(e),
    }
}

fn status_col(r: &Row<'_>, idx: usize) -> rusqlite::Result<JobStatus> {
    r.get::<_, String>(idx)?
        .parse()
        .map_err(|e: String| conversion_err(idx, e))
}

fn job_from_row(r: &Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get(0)?,
        spec: json_col(r, 1)?,
        status: status_col(r, 2)?,
        device: r.get(3)?,
        attempts: r.get(4)?,
        created_ms: r.get(5)?,
        started_ms: r.get(6)?,
        finished_ms: r.get(7)?,
        lease_expires_ms: r.get(8)?,
        not_before_ms: r.get(9)?,
        exit_code: r.get(10)?,
        last_error: r.get(11)?,
        output_path: r.get(12)?,
    })
}

/// Queues `spec`, unless `max_live` jobs are already unfinished.
pub(super) fn insert_job(
    conn: &Connection,
    max_live: i64,
    spec: &JobSpec,
    now: UnixMs,
) -> StoreResult<Job> {
    spec.validate().map_err(StoreError::InvalidSpec)?;
    let live: i64 = conn.query_row(
        &format!("SELECT count(*) FROM jobs WHERE status IN {LIVE}"),
        [],
        |r| r.get(0),
    )?;
    if live >= max_live {
        return Err(StoreError::LimitReached {
            what: "queued jobs",
            limit: max_live,
            hint: "wait for some to finish, or cancel them",
        });
    }
    conn.execute(
        "INSERT INTO jobs (spec, name, status, priority, ai_account, created_ms, key)
             VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6)",
        params![
            json_text(spec)?,
            spec.name,
            spec.priority,
            spec.ai_account,
            now,
            spec.key
        ],
    )?;
    let id = conn.last_insert_rowid();
    get_job(conn, id)?.ok_or(StoreError::JobNotFound(id))
}

pub(super) fn get_job(conn: &Connection, id: JobId) -> StoreResult<Option<Job>> {
    let sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1");
    Ok(conn.query_row(&sql, [id], job_from_row).optional()?)
}

/// The oldest job with `key` that has not finished.
fn unfinished_with_key(conn: &Connection, key: &str) -> StoreResult<Option<Job>> {
    let sql = format!(
        "SELECT {JOB_COLUMNS} FROM jobs WHERE key = ?1 AND status IN {LIVE} ORDER BY id LIMIT 1"
    );
    Ok(conn.query_row(&sql, [key], job_from_row).optional()?)
}
