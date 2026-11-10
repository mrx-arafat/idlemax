//! Adapter: commit an entire advisor plan, including queued jobs and events, atomically.

use idlemax_core::control::plan_control;
use idlemax_core::protocol::ControlRequest;
use idlemax_core::types::{ControlAction, ControlScope, EventKind, JobId, JobStatus, UnixMs};

use super::super::{events::insert_event, jobs, Store, StoreError, StoreResult};
use crate::advisor::AdviceChanges;

impl Store {
    pub fn persist_advice(
        &mut self,
        changes: &AdviceChanges,
        origin: &str,
        job: Option<JobId>,
        now: UnixMs,
    ) -> StoreResult<()> {
        let caps = self.caps;
        let tx = self.conn.transaction()?;
        let event = |kind, job, message: &str| {
            insert_event(&tx, caps.events, kind, None, job, message, now)
        };
        for id in &changes.close {
            tx.execute(
                "UPDATE recommendations SET status = 'dismissed' WHERE id = ?1 AND status = 'open'",
                [id],
            )?;
        }
        for rec in &changes.insert {
            super::insert_recommendation(&tx, rec)?;
        }
        let cancelled = cancel_stale(&tx, caps.events, &changes.cancel, now)?;
        let mut queued = 0;
        for spec in &changes.queue {
            if let Some(key) = &spec.key {
                if jobs::unfinished_with_key(&tx, key)?.is_some() {
                    continue;
                }
            }
            let added = jobs::insert_job(&tx, caps.live_jobs, spec, now)?;
            let msg = format!("{} queued (priority {})", spec.name, spec.priority);
            event(EventKind::JobQueued, Some(added.id), &msg)?;
            queued += 1;
        }
        let msg = format!("advice {origin}: {} new, {} closed, {queued} job(s) queued, {cancelled} stale cancelled", changes.insert.len(), changes.close.len());
        event(EventKind::Advice, job, &msg)?;
        tx.commit()?;
        Ok(())
    }
}

fn cancel_stale(
    conn: &rusqlite::Connection,
    keep: i64,
    ids: &[JobId],
    now: UnixMs,
) -> StoreResult<usize> {
    let event = |kind, job, message: &str| insert_event(conn, keep, kind, None, job, message, now);
    let mut cancelled = 0;
    for &id in ids {
        let Some(before) = jobs::get_job(conn, id)?.filter(|j| j.status == JobStatus::Queued)
        else {
            continue;
        };
        let req = ControlRequest {
            scope: ControlScope::Job(id),
            action: ControlAction::Cancel,
            reason: Some("stale advisor AI pass".into()),
        };
        let plan =
            plan_control(&req, &[before], now).map_err(|e| StoreError::Invalid(e.to_string()))?;
        jobs::update_jobs(conn, &plan.jobs)?;
        event(EventKind::Control, Some(id), &plan.summary)?;
        for after in &plan.jobs {
            let msg = format!(
                "{}: {}: {}",
                after.spec.name,
                after.status,
                after.last_error.as_deref().unwrap_or_default()
            );
            event(EventKind::JobCancelled, Some(id), &msg)?;
        }
        cancelled += 1;
    }
    Ok(cancelled)
}
