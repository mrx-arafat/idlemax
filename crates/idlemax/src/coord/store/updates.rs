//! Agent job updates applied exactly once (adapter). One transaction holds the
//! (device, seq) dedup row with the job state, quota side effect and events the update
//! causes, so a crash or a replay never applies half of it or all of it twice.

use idlemax_core::types::{DeviceId, EventKind, Job, JobId, UnixMs};
use rusqlite::params;

use super::events::insert_event;
use super::jobs::update_jobs;
use super::quota::upsert_limit_hit;
use super::{u64_to_sql, Store, StoreResult};

/// An event row to append.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: EventKind,
    pub device: Option<DeviceId>,
    pub job: Option<JobId>,
    pub message: String,
}

/// Everything one agent update changes, decided by the domain.
#[derive(Debug, Default)]
pub struct UpdateWrites<'a> {
    /// AI account that hit a usage limit.
    pub limit_hit: Option<&'a str>,
    /// The job's new state, when the update moves it.
    pub job: Option<&'a Job>,
    pub events: &'a [NewEvent],
}

impl Store {
    /// Applies `writes` and records `(device, seq)` in one transaction. Returns false and
    /// writes nothing when that update was applied before. `seq` 0 (an old agent) is
    /// never deduplicated.
    pub fn commit_update(
        &mut self,
        device: &str,
        seq: u64,
        writes: &UpdateWrites<'_>,
        now: UnixMs,
    ) -> StoreResult<bool> {
        let keep_events = self.caps.events;
        let tx = self.conn.transaction()?;
        if seq != 0 {
            let fresh = tx.execute(
                "INSERT INTO applied_updates (device, seq, at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT (device, seq) DO NOTHING",
                params![device, u64_to_sql(seq), now],
            )?;
            if fresh == 0 {
                return Ok(false);
            }
        }
        if let Some(account) = writes.limit_hit {
            upsert_limit_hit(&tx, account, now)?;
        }
        if let Some(job) = writes.job {
            update_jobs(&tx, std::slice::from_ref(job))?;
        }
        for e in writes.events {
            insert_event(
                &tx,
                keep_events,
                e.kind,
                e.device.as_ref(),
                e.job,
                &e.message,
                now,
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
}
