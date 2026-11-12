//! Dispatcher rows (adapter): the newest dispatch inputs per device and each project's base
//! check. Pass rows and task judgments join here in a later step. The domain decides; this file persists.

use idlemax_core::dispatch::check::BaseCheck;
use idlemax_core::dispatch::inputs::{DispatchInputs, StoredRepo};
use idlemax_core::types::UnixMs;
use rusqlite::params;

use super::{
    enum_col, enum_text, json_col, json_text, Store, StoreError, StoreResult, HISTORY_RETENTION_MS,
};

/// Largest dispatch-inputs document kept per device (a real one is a few KiB).
const MAX_INPUT_BYTES: usize = 256 * 1024;

impl Store {
    /// Replaces the device's copy and drops copies of devices silent for 30 days.
    pub fn put_dispatch_inputs(&mut self, inputs: &DispatchInputs, now: UnixMs) -> StoreResult<()> {
        if inputs.device.trim().is_empty() {
            return Err(StoreError::Invalid(
                "dispatch inputs device is empty".into(),
            ));
        }
        let payload = json_text(inputs)?;
        if payload.len() > MAX_INPUT_BYTES {
            return Err(StoreError::Invalid(format!(
                "dispatch inputs are over {MAX_INPUT_BYTES} bytes"
            )));
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO dispatch_inputs (device, received_ms, payload) VALUES (?1, ?2, ?3)
             ON CONFLICT (device) DO UPDATE SET
               received_ms = excluded.received_ms, payload = excluded.payload",
            params![inputs.device, now, payload],
        )?;
        tx.execute(
            "DELETE FROM dispatch_inputs WHERE received_ms < ?1",
            [now.saturating_sub(HISTORY_RETENTION_MS)],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every repository of every device's newest copy, by device.
    pub fn dispatch_inputs(&self) -> StoreResult<Vec<StoredRepo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT received_ms, payload FROM dispatch_inputs ORDER BY device")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, UnixMs>(0)?, json_col::<DispatchInputs>(r, 1)?))
        })?;
        let mut repos = Vec::new();
        for row in rows {
            let (received_ms, inputs) = row?;
            repos.extend(inputs.repos.into_iter().map(|repo| StoredRepo {
                device: inputs.device.clone(),
                received_ms,
                repo,
            }));
        }
        Ok(repos)
    }

    /// Every project's current base check.
    pub fn base_checks(&self) -> StoreResult<Vec<BaseCheck>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT project, base_commit, phase, job, passed, detail, finished_ms
             FROM base_checks ORDER BY project",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(BaseCheck {
                project: r.get(0)?,
                base_commit: r.get(1)?,
                phase: enum_col(r, 2)?,
                job: r.get(3)?,
                passed: r.get(4)?,
                detail: r.get(5)?,
                finished_ms: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Replaces the project's record: a new base commit starts it over.
    pub fn put_base_check(&mut self, c: &BaseCheck) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO base_checks (project, base_commit, phase, job, passed, detail, finished_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (project) DO UPDATE SET
               base_commit = excluded.base_commit, phase = excluded.phase, job = excluded.job,
               passed = excluded.passed, detail = excluded.detail,
               finished_ms = excluded.finished_ms",
            params![
                c.project,
                c.base_commit,
                enum_text(&c.phase)?,
                c.job,
                c.passed,
                c.detail,
                c.finished_ms
            ],
        )?;
        Ok(())
    }

    /// Forgets a project's record, so its next check starts fresh.
    pub fn delete_base_check(&mut self, project: i64) -> StoreResult<()> {
        self.conn
            .execute("DELETE FROM base_checks WHERE project = ?1", [project])?;
        Ok(())
    }
}
