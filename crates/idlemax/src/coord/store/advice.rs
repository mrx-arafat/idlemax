//! Advisor recommendations (adapter). The advisor decides what to insert or close;
//! the store only persists.

use idlemax_core::types::{Recommendation, RecommendationStatus};
use rusqlite::{params, OptionalExtension, Row};

use super::{
    enum_col, enum_text, json_col, json_text, opt_json_col, sql_limit, Store, StoreResult,
};

mod refresh;

const COLUMNS: &str =
    "id, at_ms, key, kind, title, why, evidence, suggested_job, confidence, source, status";

impl Store {
    /// Queues the job and marks open advice accepted in one transaction.
    pub fn accept_advice(
        &mut self,
        id: i64,
        spec: &idlemax_core::types::JobSpec,
        now: idlemax_core::types::UnixMs,
    ) -> StoreResult<idlemax_core::types::Job> {
        let live_jobs = self.caps.live_jobs;
        let tx = self.conn.transaction()?;
        let job = super::jobs::insert_job(&tx, live_jobs, spec, now)?;
        let changed = tx.execute(
            "UPDATE recommendations SET status = 'accepted' WHERE id = ?1 AND status = 'open'",
            [id],
        )?;
        if changed != 1 {
            return Err(super::StoreError::Invalid(format!(
                "advice {id} is not open"
            )));
        }
        tx.commit()?;
        Ok(job)
    }

    /// Stores a recommendation under a new id (its `id` field is ignored) and returns the id.
    #[cfg(test)]
    pub fn insert_recommendation(&mut self, rec: &Recommendation) -> StoreResult<i64> {
        insert_recommendation(&self.conn, rec)
    }

    /// Open ones first, then newest; `limit` 0 means all.
    pub fn list_recommendations(&self, limit: u32) -> StoreResult<Vec<Recommendation>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM recommendations
             ORDER BY status = 'open' DESC, at_ms DESC, id DESC LIMIT ?1"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([sql_limit(limit)], rec_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_recommendation(&self, id: i64) -> StoreResult<Option<Recommendation>> {
        let sql = format!("SELECT {COLUMNS} FROM recommendations WHERE id = ?1");
        Ok(self.conn.query_row(&sql, [id], rec_from_row).optional()?)
    }

    /// Returns the updated recommendation, or None when the id is unknown.
    pub fn set_recommendation_status(
        &mut self,
        id: i64,
        status: RecommendationStatus,
    ) -> StoreResult<Option<Recommendation>> {
        self.conn.execute(
            "UPDATE recommendations SET status = ?2 WHERE id = ?1 AND status = 'open'",
            params![id, enum_text(&status)?],
        )?;
        self.get_recommendation(id)
    }
}

fn insert_recommendation(conn: &rusqlite::Connection, rec: &Recommendation) -> StoreResult<i64> {
    conn.execute(
        "INSERT INTO recommendations (at_ms, key, kind, title, why, evidence,
               suggested_job, confidence, source, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            rec.at_ms,
            rec.key,
            enum_text(&rec.kind)?,
            rec.title,
            rec.why,
            json_text(&rec.evidence)?,
            rec.suggested_job.as_ref().map(json_text).transpose()?,
            rec.confidence,
            enum_text(&rec.source)?,
            enum_text(&rec.status)?,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn rec_from_row(r: &Row<'_>) -> rusqlite::Result<Recommendation> {
    Ok(Recommendation {
        id: r.get(0)?,
        at_ms: r.get(1)?,
        key: r.get(2)?,
        kind: enum_col(r, 3)?,
        title: r.get(4)?,
        why: r.get(5)?,
        evidence: json_col(r, 6)?,
        suggested_job: opt_json_col(r, 7)?,
        confidence: r.get(8)?,
        source: enum_col(r, 9)?,
        status: enum_col(r, 10)?,
    })
}
