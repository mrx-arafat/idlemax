//! Recurring-job schedules (adapter). The domain decides when one is due;
//! the store persists schedules and queues a fire's job and records it in one transaction.

use idlemax_core::schedule::{Every, Schedule, ScheduleId};
use idlemax_core::types::{EventKind, Job, JobSpec, UnixMs};
use rusqlite::{params, OptionalExtension, Row};

use super::events::insert_event;
use super::{
    conversion_err, json_col, json_text, u64_from_sql, u64_to_sql, Store, StoreError, StoreResult,
};

const COLUMNS: &str = "id, spec, every_secs, paused, created_ms, last_fired_ms, last_job_id";

impl Store {
    /// Stores a validated spec as a new schedule. The name is unique, so adding the same
    /// job twice cannot queue it twice per interval.
    pub fn insert_schedule(
        &mut self,
        spec: &JobSpec,
        every: Every,
        now: UnixMs,
    ) -> StoreResult<Schedule> {
        spec.validate().map_err(StoreError::InvalidSpec)?;
        if let Some(id) = self.schedule_id_by_name(&spec.name)? {
            return Err(StoreError::ScheduleExists {
                name: spec.name.clone(),
                id,
            });
        }
        self.conn.execute(
            "INSERT INTO schedules (name, spec, every_secs, created_ms) VALUES (?1, ?2, ?3, ?4)",
            params![spec.name, json_text(spec)?, u64_to_sql(every.secs()), now],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_schedule(id)?
            .ok_or(StoreError::ScheduleNotFound(id))
    }

    pub fn get_schedule(&self, id: ScheduleId) -> StoreResult<Option<Schedule>> {
        let sql = format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1");
        Ok(self
            .conn
            .query_row(&sql, [id], schedule_from_row)
            .optional()?)
    }

    /// Oldest first.
    pub fn list_schedules(&self) -> StoreResult<Vec<Schedule>> {
        let sql = format!("SELECT {COLUMNS} FROM schedules ORDER BY id");
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], schedule_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Returns the schedule as it was, or None when the id is unknown. Its last job is untouched.
    pub fn remove_schedule(&mut self, id: ScheduleId) -> StoreResult<Option<Schedule>> {
        let Some(schedule) = self.get_schedule(id)? else {
            return Ok(None);
        };
        self.conn
            .execute("DELETE FROM schedules WHERE id = ?1", [id])?;
        Ok(Some(schedule))
    }

    /// Returns the updated schedule, or None when the id is unknown.
    pub fn set_schedule_paused(
        &mut self,
        id: ScheduleId,
        paused: bool,
    ) -> StoreResult<Option<Schedule>> {
        self.conn.execute(
            "UPDATE schedules SET paused = ?2 WHERE id = ?1",
            params![id, paused],
        )?;
        self.get_schedule(id)
    }

    /// Queues the schedule's job, links it as the last one and logs both events, atomically,
    /// so a crash can never leave a job the schedule does not know about.
    pub fn fire_schedule(&mut self, schedule: &Schedule, now: UnixMs) -> StoreResult<Job> {
        let (keep_events, live_jobs) = (self.caps.events, self.caps.live_jobs);
        let tx = self.conn.transaction()?;
        let job = super::jobs::insert_job(&tx, live_jobs, &schedule.spec, now)?;
        let changed = tx.execute(
            "UPDATE schedules SET last_fired_ms = ?2, last_job_id = ?3 WHERE id = ?1",
            params![schedule.id, now, job.id],
        )?;
        if changed != 1 {
            return Err(StoreError::ScheduleNotFound(schedule.id));
        }
        let queued = format!("{} queued (priority {})", job.spec.name, job.spec.priority);
        insert_event(
            &tx,
            keep_events,
            EventKind::JobQueued,
            None,
            Some(job.id),
            &queued,
            now,
        )?;
        let fired = format!(
            "schedule {} ({}) queued job {}, repeats every {}",
            schedule.id, job.spec.name, job.id, schedule.every
        );
        insert_event(
            &tx,
            keep_events,
            EventKind::ScheduleFired,
            None,
            Some(job.id),
            &fired,
            now,
        )?;
        tx.commit()?;
        Ok(job)
    }

    /// Starts a new interval without a job, because the fire could not queue one.
    /// Logs `reason` as an error so the owner sees why the job did not appear.
    pub fn defer_schedule(
        &mut self,
        schedule: &Schedule,
        reason: &str,
        now: UnixMs,
    ) -> StoreResult<()> {
        let keep_events = self.caps.events;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE schedules SET last_fired_ms = ?2 WHERE id = ?1",
            params![schedule.id, now],
        )?;
        let msg = format!(
            "schedule {} ({}) could not queue a job: {reason}; trying again in {}",
            schedule.id, schedule.spec.name, schedule.every
        );
        insert_event(&tx, keep_events, EventKind::Error, None, None, &msg, now)?;
        tx.commit()?;
        Ok(())
    }

    fn schedule_id_by_name(&self, name: &str) -> StoreResult<Option<ScheduleId>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM schedules WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }
}

fn schedule_from_row(r: &Row<'_>) -> rusqlite::Result<Schedule> {
    let every = Every::from_secs(u64_from_sql(r.get(2)?)).map_err(|e| conversion_err(2, e))?;
    Ok(Schedule {
        id: r.get(0)?,
        spec: json_col(r, 1)?,
        every,
        paused: r.get(3)?,
        created_ms: r.get(4)?,
        last_fired_ms: r.get(5)?,
        last_job: r.get(6)?,
    })
}
