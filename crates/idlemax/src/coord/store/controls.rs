//! Coordinator pause flags per control scope (adapter).

use idlemax_core::types::ControlScope;
use rusqlite::params;

use super::{Store, StoreError, StoreResult};

impl Store {
    /// Pauses or un-pauses one scope. Idempotent.
    pub fn set_paused(&mut self, scope: &ControlScope, paused: bool) -> StoreResult<()> {
        let (kind, id) = scope_key(scope);
        let sql = if paused {
            "INSERT INTO controls (scope_type, scope_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING"
        } else {
            "DELETE FROM controls WHERE scope_type = ?1 AND scope_id = ?2"
        };
        self.conn.execute(sql, params![kind, id])?;
        Ok(())
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
