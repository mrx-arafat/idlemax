//! Budget reads (adapter): worker Runs as cost samples, with the after reading taken from the
//! snapshots stored after each Run's end. Reads only.

use idlemax_core::quota::cost::RunSample;
use idlemax_core::quota::owner_use::AccountDay;
use idlemax_core::types::{QuotaSnapshot, QuotaWindowKind, UnixMs};
use idlemax_core::worker::run::Run;
use rusqlite::{params, OptionalExtension};

use super::quota::snapshot_from_row;
use super::{json_text, Store, StoreResult};

#[cfg(test)]
mod tests;

// The Budget gathering of dispatcher Spec 3 Task 6 reads these.
#[cfg_attr(not(test), allow(dead_code))]
impl Store {
    /// Runs started at or after `since_ms` as cost samples. The after reading of each window is
    /// the oldest snapshot of the account taken at or after the Run's end: the Run's own
    /// `quota_after` is the newest it held at its end, which is always older than the end.
    pub fn run_samples_since(&self, since_ms: UnixMs) -> StoreResult<Vec<RunSample>> {
        let runs = self.runs_where("start_ms >= ?1", params![since_ms])?;
        runs.into_iter().map(|r| self.sample_of(r)).collect()
    }

    fn sample_of(&self, run: Run) -> StoreResult<RunSample> {
        let mut after = Vec::new();
        if let Some(end) = run.end_ms {
            for before in &run.quota_before {
                after.extend(self.first_snapshot_at(&run.account, before.window, end)?);
            }
        }
        Ok(RunSample {
            provider: run.provider,
            account: run.account,
            model: run.model,
            task_type: run.source,
            started_ms: run.start_ms,
            ended_ms: run.end_ms,
            launches: run.launches,
            before: run.quota_before,
            after,
        })
    }

    /// The oldest snapshot of one account window taken at or after `at_ms`.
    fn first_snapshot_at(
        &self,
        account: &str,
        window: QuotaWindowKind,
        at_ms: UnixMs,
    ) -> StoreResult<Option<QuotaSnapshot>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT account, quota_window, used_pct, resets_at_ms, source, at_ms
                 FROM quota_snapshots WHERE account = ?1 AND quota_window = ?2 AND at_ms >= ?3
                 ORDER BY at_ms LIMIT 1",
            )?
            .query_row(
                params![account, json_text(&window)?, at_ms],
                snapshot_from_row,
            )
            .optional()?)
    }
}

// The Budget of dispatcher Spec 3 Tasks 4 and 6 writes and reads account days here.
#[cfg_attr(not(test), allow(dead_code))]
impl Store {
    /// Weekly snapshots of `account` taken at or after `since_ms`, oldest first.
    pub fn weekly_snapshots(
        &self,
        account: &str,
        since_ms: UnixMs,
    ) -> StoreResult<Vec<QuotaSnapshot>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT account, quota_window, used_pct, resets_at_ms, source, at_ms
             FROM quota_snapshots WHERE account = ?1 AND quota_window = ?2 AND at_ms >= ?3
             ORDER BY at_ms",
        )?;
        let weekly = json_text(&QuotaWindowKind::Weekly)?;
        let rows = stmt.query_map(params![account, weekly, since_ms], snapshot_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Closes a day: sets its four totals and keeps the Budget it had.
    pub fn close_day(&mut self, day: &AccountDay) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO quota_days (account, day_start_ms, total_pct, idlemax_pct, owner_pct,
               runs, closed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
             ON CONFLICT (account, day_start_ms) DO UPDATE SET
               total_pct = excluded.total_pct, idlemax_pct = excluded.idlemax_pct,
               owner_pct = excluded.owner_pct, runs = excluded.runs, closed = 1",
            params![
                day.account,
                day.day_start_ms,
                day.total_pct,
                day.idlemax_pct,
                day.owner_pct,
                day.runs
            ],
        )?;
        Ok(())
    }

    /// Records what an open day's Budget says now; a closed day keeps what it had.
    pub fn record_open_day(
        &mut self,
        account: &str,
        day_start_ms: UnixMs,
        spendable_pct: Option<f64>,
        basis: &str,
        why: &str,
    ) -> StoreResult<()> {
        self.conn.execute(
            "INSERT INTO quota_days (account, day_start_ms, spendable_pct, basis, why)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (account, day_start_ms) DO UPDATE SET
               spendable_pct = excluded.spendable_pct, basis = excluded.basis, why = excluded.why
             WHERE closed = 0",
            params![account, day_start_ms, spendable_pct, basis, why],
        )?;
        Ok(())
    }

    /// The account's days that started at or after `since_ms`, newest first.
    pub fn budget_days(&self, account: &str, since_ms: UnixMs) -> StoreResult<Vec<AccountDay>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT account, day_start_ms, spendable_pct, basis, why, total_pct, idlemax_pct,
               owner_pct, runs, closed
             FROM quota_days WHERE account = ?1 AND day_start_ms >= ?2
             ORDER BY day_start_ms DESC",
        )?;
        let rows = stmt.query_map(params![account, since_ms], |r| {
            Ok(AccountDay {
                account: r.get(0)?,
                day_start_ms: r.get(1)?,
                spendable_pct: r.get(2)?,
                basis: r.get(3)?,
                why: r.get(4)?,
                total_pct: r.get(5)?,
                idlemax_pct: r.get(6)?,
                owner_pct: r.get(7)?,
                runs: r.get(8)?,
                closed: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The account's Runs with a session id that started in [from_ms, to_ms).
    pub fn runs_started_in(
        &self,
        account: &str,
        from_ms: UnixMs,
        to_ms: UnixMs,
    ) -> StoreResult<u32> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM runs
             WHERE account = ?1 AND session IS NOT NULL AND start_ms >= ?2 AND start_ms < ?3",
            params![account, from_ms, to_ms],
            |r| r.get(0),
        )?)
    }
}

impl Store {
    /// Per task id, how many of its Runs ended with the verdict `rejected` (D10.6).
    pub fn failed_checks(&self) -> StoreResult<Vec<(String, u32)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT task, count(*) FROM runs WHERE verdict = 'rejected' GROUP BY task ORDER BY task",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
