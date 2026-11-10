//! Coordinator pause flags per control scope (adapter).

use idlemax_core::report::PauseSpan;
use idlemax_core::types::{ControlScope, UnixMs};
use rusqlite::params;

use super::{Store, StoreError, StoreResult};

impl Store {
    /// Persists a pause flag and its changed jobs atomically.
    pub fn apply_control(
        &mut self,
        plan: &idlemax_core::control::ControlPlan,
        now: UnixMs,
    ) -> StoreResult<()> {
        let (max_flags, max_spans) = (self.caps.controls, self.caps.pause_spans);
        let tx = self.conn.transaction()?;
        if let Some((scope, paused)) = &plan.pause {
            set_paused(&tx, max_flags, scope, *paused)?;
            record_pause(&tx, max_spans, scope, *paused, now)?;
        }
        super::jobs::update_jobs(&tx, &plan.jobs)?;
        tx.commit()?;
        Ok(())
    }

    /// Pauses in force at any time at or after `since_ms`, oldest first.
    pub fn pause_spans_since(&self, since_ms: UnixMs) -> StoreResult<Vec<PauseSpan>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT scope_type, scope_id, start_ms, end_ms FROM pause_spans
             WHERE end_ms IS NULL OR end_ms >= ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([since_ms], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get(2)?,
                r.get(3)?,
            ))
        })?;
        rows.map(|row| {
            let (kind, id, from_ms, to_ms) = row?;
            Ok(PauseSpan {
                scope: scope_from(&kind, id)?,
                from_ms,
                to_ms,
            })
        })
        .collect()
    }

    pub fn paused_scopes(&self) -> StoreResult<Vec<ControlScope>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT scope_type, scope_id FROM controls ORDER BY scope_type, scope_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|row| {
            let (kind, id) = row?;
            scope_from(&kind, id)
        })
        .collect()
    }
}

fn scope_key(scope: &ControlScope) -> (&'static str, String) {
    match scope {
        ControlScope::Global => ("global", String::new()),
        ControlScope::Device(id) => ("device", id.clone()),
        ControlScope::Account(name) => ("account", name.clone()),
        ControlScope::Job(id) => ("job", id.to_string()),
    }
}

fn scope_from(kind: &str, id: String) -> StoreResult<ControlScope> {
    match kind {
        "global" => Ok(ControlScope::Global),
        "device" => Ok(ControlScope::Device(id)),
        "account" => Ok(ControlScope::Account(id)),
        "job" => id
            .parse()
            .map(ControlScope::Job)
            .map_err(|_| StoreError::Corrupt(format!("job control id {id:?}"))),
        other => Err(StoreError::Corrupt(format!("control scope type {other:?}"))),
    }
}

fn set_paused(
    conn: &rusqlite::Connection,
    max_flags: i64,
    scope: &ControlScope,
    paused: bool,
) -> StoreResult<()> {
    let (kind, id) = scope_key(scope);
    if paused {
        let known: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM controls WHERE scope_type = ?1 AND scope_id = ?2)",
            params![kind, id],
            |r| r.get(0),
        )?;
        let flags: i64 = conn.query_row("SELECT count(*) FROM controls", [], |r| r.get(0))?;
        if !known && flags >= max_flags {
            return Err(StoreError::LimitReached {
                what: "pause flags",
                limit: max_flags,
                hint: "resume one first",
            });
        }
    }
    let sql = if paused {
        "INSERT INTO controls (scope_type, scope_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM controls WHERE scope_type = ?1 AND scope_id = ?2"
    };
    conn.execute(sql, params![kind, id])?;
    Ok(())
}

/// Keeps the history of pauses: a pause opens a span unless one is open, a resume closes it.
/// Only the newest `max_spans` rows are kept, closed ones going first, so pausing and
/// resuming over and over cannot grow the table.
fn record_pause(
    conn: &rusqlite::Connection,
    max_spans: i64,
    scope: &ControlScope,
    paused: bool,
    now: UnixMs,
) -> StoreResult<()> {
    let (kind, id) = scope_key(scope);
    if paused {
        conn.execute(
            "INSERT INTO pause_spans (scope_type, scope_id, start_ms)
             SELECT ?1, ?2, ?3 WHERE NOT EXISTS (
               SELECT 1 FROM pause_spans
               WHERE scope_type = ?1 AND scope_id = ?2 AND end_ms IS NULL)",
            params![kind, id, now],
        )?;
    } else {
        conn.execute(
            "UPDATE pause_spans SET end_ms = max(start_ms, ?3)
             WHERE scope_type = ?1 AND scope_id = ?2 AND end_ms IS NULL",
            params![kind, id, now],
        )?;
    }
    conn.execute(
        "DELETE FROM pause_spans WHERE end_ms IS NOT NULL
           AND id NOT IN (SELECT id FROM pause_spans ORDER BY id DESC LIMIT ?1)",
        [max_spans],
    )?;
    Ok(())
}
