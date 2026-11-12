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

// The dispatcher's launch step, its job updates and the owner's review use these (dispatcher
// Spec 2 Tasks 3, 9, 10 and 11).
#[cfg_attr(not(test), allow(dead_code))]
impl Store {
    /// Stores a new Run with what its relaunches need and returns it with its id.
    pub fn insert_run(&mut self, run: &Run, launch: &RunLaunch) -> StoreResult<Run> {
        self.conn.execute(
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
        let id = self.conn.last_insert_rowid();
        self.get_run(id)?
            .ok_or_else(|| StoreError::Corrupt(format!("run {id}")))
    }

    /// Writes `run` only while its stored state is still `expect`; false when it moved, so
    /// a late agent update never overwrites the owner's stop.
    pub fn update_run(&mut self, run: &Run, expect: RunState) -> StoreResult<bool> {
        update_run(&self.conn, run, expect)
    }

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

    /// Runs of `account` started at or after `since_ms`, oldest first.
    pub fn runs_of_account(&self, account: &str, since_ms: UnixMs) -> StoreResult<Vec<Run>> {
        self.runs_where(
            "account = ?1 AND start_ms >= ?2",
            params![account, since_ms],
        )
    }

    /// Every Run of `project`, oldest first.
    pub fn runs_of_project(&self, project: &str) -> StoreResult<Vec<Run>> {
        self.runs_where("project = ?1", params![project])
    }

    fn runs_where(&self, filter: &str, args: impl rusqlite::Params) -> StoreResult<Vec<Run>> {
        let sql = format!("SELECT {COLUMNS} FROM runs WHERE {filter} ORDER BY start_ms, id");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(args, run_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
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
        spec.validate().map_err(StoreError::InvalidSpec)?;
        let live_jobs = self.caps.live_jobs;
        let tx = self.conn.transaction()?;
        if let Some(key) = &spec.key {
            if let Some(job) = unfinished_with_key(&tx, key)? {
                return Ok(Added::Existing(job));
            }
        }
        check_live_cap(&tx, live_jobs)?;
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
        let job: Job = get_job(&tx, id)?.ok_or(StoreError::JobNotFound(id))?;
        tx.commit()?;
        Ok(Added::New(job))
    }
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
mod tests {
    use idlemax_core::types::{CostClass, JobStatus};
    use idlemax_core::worker::WorkerSpec;

    use super::super::schema;
    use super::*;

    const T0: UnixMs = 1_700_000_000_000;

    fn ai_spec(name: &str, worker: bool) -> JobSpec {
        JobSpec {
            name: name.into(),
            command: vec!["idlemax-worker".into(), "claude".into()],
            cost: CostClass::Ai,
            ai_account: Some("max".into()),
            worker: worker.then(WorkerSpec::default),
            ..Default::default()
        }
    }

    fn run(project: &str, state: RunState) -> Run {
        Run {
            device: "mac".into(),
            project: project.into(),
            task: "roadmap:1".into(),
            account: "max".into(),
            model: "sonnet".into(),
            reason: "the oldest ready task".into(),
            state,
            start_ms: T0,
            ..Default::default()
        }
    }

    #[test]
    fn a_v8_store_migrates_and_keeps_its_jobs_as_plain_ones() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &schema::MIGRATIONS[..8] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 8).unwrap();
        conn.execute(
            "INSERT INTO jobs (spec, name, status, priority, created_ms)
             VALUES ('{\"name\":\"old\",\"command\":[\"true\"]}', 'old', 'queued', 0, ?1)",
            [T0],
        )
        .unwrap();
        let s = Store::init(conn, false).unwrap();
        let version: i64 = s
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, schema::MIGRATIONS.len() as i64);
        assert_eq!(s.get_job(1).unwrap().unwrap().spec.name, "old");
        let worker: i64 = s
            .conn
            .query_row("SELECT worker FROM jobs WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(worker, 0);
    }

    #[test]
    fn worker_jobs_stand_outside_the_one_ai_lease_rule() {
        let mut s = Store::open_in_memory().unwrap();
        for name in ["w1", "w2"] {
            let added = s
                .insert_leased_job(&ai_spec(name, true), "mac", T0 + 60_000, T0)
                .unwrap();
            assert_eq!(added.into_job().status, JobStatus::Leased);
        }
        let plain = s.insert_job(&ai_spec("p1", false), T0).unwrap();
        assert!(s
            .lease_job(plain.id, "mac", T0 + 60_000, T0)
            .unwrap()
            .is_some());
        let second = s.insert_job(&ai_spec("p2", false), T0).unwrap();
        assert!(matches!(
            s.lease_job(second.id, "mac", T0 + 60_000, T0),
            Err(StoreError::AccountLeased(_))
        ));
        assert_eq!(s.ai_leased_accounts().unwrap(), ["max"]);
    }

    #[test]
    fn a_started_worker_job_is_not_an_ai_job_start() {
        let mut s = Store::open_in_memory().unwrap();
        let mut job = s
            .insert_leased_job(&ai_spec("w", true), "mac", T0 + 60_000, T0)
            .unwrap()
            .into_job();
        job.status = JobStatus::Running;
        job.started_ms = Some(T0);
        s.update_jobs(&[job]).unwrap();
        assert!(s.ai_job_starts(T0 - 1).unwrap().is_empty());
        assert!(s.ai_leased_accounts().unwrap().is_empty());
    }

    #[test]
    fn a_live_key_returns_its_job_and_inserts_nothing() {
        let mut s = Store::open_in_memory().unwrap();
        let spec = JobSpec {
            key: Some("run-1".into()),
            ..ai_spec("w", true)
        };
        let first = s
            .insert_leased_job(&spec, "mac", T0 + 60_000, T0)
            .unwrap()
            .into_job();
        let again = s.insert_leased_job(&spec, "mac", T0 + 60_000, T0).unwrap();
        assert_eq!(again, Added::Existing(first));
        assert_eq!(s.list_jobs(None, 0).unwrap().len(), 1);
    }

    #[test]
    fn update_run_writes_only_while_the_stored_state_matches() {
        let mut s = Store::open_in_memory().unwrap();
        let stored = s
            .insert_run(&run("demo", RunState::Starting), &RunLaunch::default())
            .unwrap();
        let mut next = stored.clone();
        next.state = RunState::Running;
        next.session = Some("abc".into());
        assert!(s.update_run(&next, RunState::Starting).unwrap());
        assert_eq!(s.get_run(stored.id).unwrap().unwrap(), next);

        let mut late = next.clone();
        late.state = RunState::Checking;
        assert!(!s.update_run(&late, RunState::Starting).unwrap());
        assert_eq!(s.get_run(stored.id).unwrap().unwrap(), next);
    }

    #[test]
    fn runs_are_listed_by_state_and_project_newest_first() {
        let mut s = Store::open_in_memory().unwrap();
        let launch = RunLaunch::default();
        let a = s
            .insert_run(&run("a", RunState::Starting), &launch)
            .unwrap();
        let b = s
            .insert_run(&run("b", RunState::Starting), &launch)
            .unwrap();
        let c = s.insert_run(&run("a", RunState::Shed), &launch).unwrap();
        let ids = |runs: Vec<Run>| runs.into_iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(s.list_runs(None, None, 0).unwrap()), [c.id, b.id, a.id]);
        assert_eq!(ids(s.list_runs(None, Some("a"), 0).unwrap()), [c.id, a.id]);
        assert_eq!(
            ids(s.list_runs(Some(RunState::Starting), Some("a"), 0).unwrap()),
            [a.id]
        );
        assert_eq!(ids(s.list_runs(None, None, 1).unwrap()), [c.id]);
        assert_eq!(ids(s.runs_of_project("a").unwrap()), [a.id, c.id]);
        assert_eq!(s.runs_of_account("max", T0).unwrap().len(), 3);
        assert!(s.runs_of_account("max", T0 + 1).unwrap().is_empty());
    }

    #[test]
    fn insert_run_stores_every_field_it_is_given() {
        let mut s = Store::open_in_memory().unwrap();
        let mut given = run("demo", RunState::Starting);
        given.session = Some("sess".into());
        given.updated_ms = T0 + 5;
        given.launches = 1;
        let stored = s.insert_run(&given, &RunLaunch::default()).unwrap();
        given.id = stored.id;
        assert_eq!(stored, given);
    }

    #[test]
    fn leased_jobs_count_against_the_live_jobs_cap() {
        let caps = super::super::Caps {
            live_jobs: 1,
            ..Default::default()
        };
        let mut s = Store::open_in_memory().unwrap().with_caps(caps);
        s.insert_leased_job(&ai_spec("w1", true), "mac", T0 + 1, T0)
            .unwrap();
        assert!(matches!(
            s.insert_leased_job(&ai_spec("w2", true), "mac", T0 + 1, T0),
            Err(StoreError::LimitReached { .. })
        ));
    }

    #[test]
    fn prune_drops_old_kept_and_discarded_runs_even_without_an_end_time() {
        let mut s = Store::open_in_memory().unwrap();
        let launch = RunLaunch::default();
        let mut kept = run("a", RunState::Kept);
        kept.end_ms = Some(T0);
        let mut discarded = run("a", RunState::Discarded);
        discarded.updated_ms = T0;
        let clean = run("a", RunState::Clean);
        for r in [&kept, &discarded, &clean] {
            s.insert_run(r, &launch).unwrap();
        }
        let report = s.prune(T0 + 91 * 86_400_000, i64::MAX).unwrap();
        assert_eq!(report.runs, 2);
        assert_eq!(s.list_runs(None, None, 0).unwrap().len(), 1);
    }
}
