//! Budget reads (adapter): worker Runs as cost samples, with the after reading taken from the
//! snapshots stored after each Run's end. Reads only.

use idlemax_core::quota::cost::RunSample;
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
