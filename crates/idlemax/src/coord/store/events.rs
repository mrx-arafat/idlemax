//! Append-only event log (adapter).

use idlemax_core::types::{DeviceId, Event, EventKind, JobId, UnixMs};
use rusqlite::{params, Connection, Row};

use super::{enum_col, enum_text, sql_limit, Store, StoreResult};

impl Store {
    pub fn append_event(
        &mut self,
        kind: EventKind,
        device: Option<&DeviceId>,
        job: Option<JobId>,
        message: &str,
        now: UnixMs,
    ) -> StoreResult<Event> {
        insert_event(&self.conn, kind, device, job, message, now)
    }

    /// True when an event of `kind` names `job`.
    pub fn job_has_event(&self, job: JobId, kind: EventKind) -> StoreResult<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM events WHERE job = ?1 AND kind = ?2)")?
            .query_row(params![job, enum_text(&kind)?], |r| r.get(0))?)
    }

    /// Newest first; `limit` 0 means all.
    pub fn events(&self, limit: u32) -> StoreResult<Vec<Event>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, at_ms, kind, device, job, message FROM events ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([sql_limit(limit)], event_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// The insert behind `append_event`, usable inside a caller's transaction.
pub(super) fn insert_event(
    conn: &Connection,
    kind: EventKind,
    device: Option<&DeviceId>,
    job: Option<JobId>,
    message: &str,
    now: UnixMs,
) -> StoreResult<Event> {
    conn.prepare_cached(
        "INSERT INTO events (at_ms, kind, device, job, message)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![now, enum_text(&kind)?, device, job, message])?;
    Ok(Event {
        id: conn.last_insert_rowid(),
        at_ms: now,
        kind,
        device: device.cloned(),
        job,
        message: message.to_owned(),
    })
}

fn event_from_row(r: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: r.get(0)?,
        at_ms: r.get(1)?,
        kind: enum_col(r, 2)?,
        device: r.get(3)?,
        job: r.get(4)?,
        message: r.get(5)?,
    })
}
