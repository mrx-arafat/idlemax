//! Notice rows (adapter): the stored notices, the keys each covers, and the finished runs
//! and plain jobs that no notice covers yet. The domain decides what a notice says.

use idlemax_core::notify::notice::{Draft, Notice, NoticeKind};
use idlemax_core::types::{Job, UnixMs};
use idlemax_core::worker::run::Run;
use rusqlite::params;

use super::jobs::{job_from_row, JOB_COLUMNS};
use super::runs::{run_from_row, COLUMNS as RUN_COLUMNS};
use super::{enum_col, enum_text, sql_limit, Store, StoreResult};

impl Store {
    pub fn is_covered(&self, key: &str) -> StoreResult<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM notice_items WHERE key = ?1)",
            [key],
            |r| r.get(0),
        )?)
    }

    /// Runs that finished with the verdict `clean` or `needs_you` since `since` and that no
    /// notice covers.
    pub fn unnoticed_runs_since(&self, since: UnixMs) -> StoreResult<Vec<Run>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE verdict IN ('clean', 'needs_you') AND COALESCE(end_ms, updated_ms) >= ?1
               AND 'run:' || id NOT IN (SELECT key FROM notice_items)
             ORDER BY id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([since], run_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Jobs that ended `succeeded`, `failed`, `killed` or `parked` since `since` and that no
    /// notice covers; the domain picks the plain ones.
    pub fn finished_jobs_since(&self, since: UnixMs) -> StoreResult<Vec<Job>> {
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM jobs
             WHERE status IN ('succeeded', 'failed', 'killed', 'parked') AND finished_ms >= ?1
               AND 'job:' || id NOT IN (SELECT key FROM notice_items)
             ORDER BY id"
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([since], job_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Runs that reached `clean` or `needs_you` in `[start, end)`.
    pub fn tasks_done_between(&self, start: UnixMs, end: UnixMs) -> StoreResult<u32> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM runs WHERE verdict IN ('clean', 'needs_you')
               AND COALESCE(end_ms, updated_ms) >= ?1 AND COALESCE(end_ms, updated_ms) < ?2",
            [start, end],
            |r| r.get(0),
        )?)
    }

    /// Runs that wait for the owner's review: `clean` and `needs_you`.
    pub fn waiting_runs(&self) -> StoreResult<u32> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM runs WHERE state IN ('clean', 'needs_you')",
            [],
            |r| r.get(0),
        )?)
    }

    /// Covers a key with no notice, such as a day whose summary had nothing to say.
    pub fn cover_key(&mut self, key: &str, now: UnixMs) -> StoreResult<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO notice_items (key, notice, at_ms, last_seen_ms)
             VALUES (?1, 0, ?2, ?2)",
            params![key, now],
        )?;
        Ok(())
    }

    /// Stores the notice and covers its keys, in one transaction.
    pub fn insert_notice(&mut self, d: &Draft, now: UnixMs) -> StoreResult<Notice> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO notices (at_ms, kind, title, body) VALUES (?1, ?2, ?3, ?4)",
            params![now, enum_text(&d.kind)?, d.title, d.body],
        )?;
        let id = tx.last_insert_rowid();
        for key in &d.keys {
            tx.execute(
                "INSERT OR REPLACE INTO notice_items (key, notice, at_ms, last_seen_ms)
                 VALUES (?1, ?2, ?3, ?3)",
                params![key, id, now],
            )?;
        }
        tx.commit()?;
        Ok(Notice {
            id,
            at_ms: now,
            kind: d.kind,
            title: d.title.clone(),
            body: d.body.clone(),
        })
    }

    /// Stamps the "needs you" keys still in the view, then releases those that were gone
    /// since `release_before`, so the condition notifies again when it returns.
    pub fn refresh_needs(
        &mut self,
        seen: &[String],
        now: UnixMs,
        release_before: UnixMs,
    ) -> StoreResult<()> {
        let tx = self.conn.transaction()?;
        for key in seen {
            tx.execute(
                "UPDATE notice_items SET last_seen_ms = ?2 WHERE key = ?1",
                params![key, now],
            )?;
        }
        tx.execute(
            "DELETE FROM notice_items WHERE last_seen_ms < ?1
               AND notice IN (SELECT id FROM notices WHERE kind = 'needs_you')",
            [release_before],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Notices with an id above `after` and no older than `since`, oldest first.
    pub fn notices_after(&self, after: i64, since: UnixMs, limit: u32) -> StoreResult<Vec<Notice>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, at_ms, kind, title, body FROM notices
             WHERE id > ?1 AND at_ms >= ?2 ORDER BY id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![after, since, sql_limit(limit)], |r| {
            Ok(Notice {
                id: r.get(0)?,
                at_ms: r.get(1)?,
                kind: enum_col::<NoticeKind>(r, 2)?,
                title: r.get(3)?,
                body: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::notify::notice::{Draft, NoticeKind};

    use super::*;

    const DAY: UnixMs = 86_400_000;
    const T0: UnixMs = 1_800_000_000_000;

    fn draft(kind: NoticeKind, key: &str) -> Draft {
        Draft {
            kind,
            title: "t".into(),
            body: "b".into(),
            keys: vec![key.into()],
        }
    }

    #[test]
    fn prune_keeps_a_key_still_seen_and_drops_old_ones_and_their_notices() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_notice(&draft(NoticeKind::NeedsYou, "device:mac:emergency"), T0)
            .unwrap();
        s.insert_notice(&draft(NoticeKind::TaskFinished, "run:1"), T0)
            .unwrap();
        let now = T0 + 31 * DAY;
        s.refresh_needs(&["device:mac:emergency".into()], now, now - DAY)
            .unwrap();
        s.prune(now, DAY).unwrap();
        assert!(s.is_covered("device:mac:emergency").unwrap());
        assert!(!s.is_covered("run:1").unwrap());
        assert!(s.notices_after(0, 0, 0).unwrap().is_empty());
    }
}
