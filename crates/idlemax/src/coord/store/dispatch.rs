//! Dispatcher rows (adapter): the newest dispatch inputs per device and each project's base
//! check, the orchestrator passes and the tasks the orchestrator sent to "needs you". The domain decides; this file persists.

use idlemax_core::dispatch::check::BaseCheck;
use idlemax_core::dispatch::inputs::{DispatchInputs, StoredRepo};
use idlemax_core::dispatch::plan::PassResult;
use idlemax_core::dispatch::task::Judgment;
use idlemax_core::types::{JobId, UnixMs};
use rusqlite::{params, OptionalExtension};

use super::{
    enum_col, enum_text, json_col, json_text, opt_json_col, Store, StoreError, StoreResult,
    HISTORY_RETENTION_MS,
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

    /// The devices whose newest feed came without a Herdr view.
    pub fn devices_without_herdr(&self) -> StoreResult<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM dispatch_inputs ORDER BY device")?;
        let rows = stmt.query_map([], |r| json_col::<DispatchInputs>(r, 0))?;
        let mut out = Vec::new();
        for inputs in rows {
            let inputs = inputs?;
            if !inputs.herdr {
                out.push(inputs.device);
            }
        }
        Ok(out)
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

    /// True when an owner stop is stored (`worker_stop` table has a row).
    pub fn has_worker_stop(&self) -> StoreResult<bool> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM worker_stop", [], |r| r.get(0))?;
        Ok(count > 0)
    }

    /// Records a queued pass: what it was offered and the inputs' fingerprint.
    pub fn insert_pass(
        &mut self,
        job: JobId,
        account: &str,
        fingerprint: &str,
        offered: &[String],
        now: UnixMs,
    ) -> StoreResult<i64> {
        self.conn.execute(
            "INSERT INTO orchestrator_passes (job, account, fingerprint, offered, queued_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job, account, fingerprint, json_text(&offered)?, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest pass row, or the newest one that has a result.
    pub fn newest_pass(&self, with_result: bool) -> StoreResult<Option<PassRow>> {
        let filter = if with_result {
            "WHERE result IS NOT NULL"
        } else {
            ""
        };
        let sql = format!(
            "SELECT {PASS_COLUMNS} FROM orchestrator_passes {filter} ORDER BY id DESC LIMIT 1"
        );
        Ok(self.conn.query_row(&sql, [], pass_from_row).optional()?)
    }

    pub fn pass_for_job(&self, job: JobId) -> StoreResult<Option<PassRow>> {
        let sql = format!("SELECT {PASS_COLUMNS} FROM orchestrator_passes WHERE job = ?1");
        Ok(self.conn.query_row(&sql, [job], pass_from_row).optional()?)
    }

    /// Stores the checked result, or why the pass gave none.
    pub fn finish_pass(
        &mut self,
        id: i64,
        outcome: Result<&PassResult, &str>,
        now: UnixMs,
    ) -> StoreResult<()> {
        let (result, error) = match outcome {
            Ok(r) => (Some(json_text(r)?), None),
            Err(e) => (None, Some(e.chars().take(500).collect::<String>())),
        };
        self.conn.execute(
            "UPDATE orchestrator_passes SET finished_ms = ?2, result = ?3, error = ?4 WHERE id = ?1",
            params![id, now, result, error],
        )?;
        Ok(())
    }

    /// Sends a task to "needs you" until it changes (A20).
    pub fn judge_task(
        &mut self,
        task: &str,
        digest: &str,
        reason: &str,
        now: UnixMs,
    ) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO task_judgments (task, digest, reason, judged_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (task) DO UPDATE SET
               digest = excluded.digest, reason = excluded.reason, judged_ms = excluded.judged_ms",
            params![task, digest, reason, now],
        )?;
        Ok(())
    }

    pub fn judgments(&self) -> StoreResult<Vec<Judgment>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT task, digest, reason FROM task_judgments ORDER BY task")?;
        let rows = stmt.query_map([], |r| {
            Ok(Judgment {
                task: r.get(0)?,
                digest: r.get(1)?,
                reason: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn delete_judgment(&mut self, task: &str) -> StoreResult<()> {
        self.conn
            .execute("DELETE FROM task_judgments WHERE task = ?1", [task])?;
        Ok(())
    }
}

/// One orchestrator pass as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct PassRow {
    pub id: i64,
    pub job: JobId,
    pub account: String,
    pub fingerprint: String,
    /// Ids of the tasks the pass was offered.
    pub offered: Vec<String>,
    pub queued_ms: UnixMs,
    pub finished_ms: Option<UnixMs>,
    pub result: Option<PassResult>,
    pub error: Option<String>,
}

const PASS_COLUMNS: &str =
    "id, job, account, fingerprint, offered, queued_ms, finished_ms, result, error";

fn pass_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PassRow> {
    Ok(PassRow {
        id: r.get(0)?,
        job: r.get(1)?,
        account: r.get(2)?,
        fingerprint: r.get(3)?,
        offered: json_col(r, 4)?,
        queued_ms: r.get(5)?,
        finished_ms: r.get(6)?,
        result: opt_json_col(r, 7)?,
        error: r.get(8)?,
    })
}
