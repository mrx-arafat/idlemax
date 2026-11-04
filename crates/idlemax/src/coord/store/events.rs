//! Append-only event log (adapter).

use idlemax_core::types::{DeviceId, Event, EventKind, JobId, UnixMs};
use rusqlite::{params, Row};

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
        self.conn
            .prepare_cached(
                "INSERT INTO events (at_ms, kind, device, job, message)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![now, enum_text(&kind)?, device, job, message])?;
        Ok(Event {
            id: self.conn.last_insert_rowid(),
            at_ms: now,
            kind,
            device: device.cloned(),
            job,
            message: message.to_owned(),
        })
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
