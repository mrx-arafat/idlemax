//! What the value report reads from the store (adapter): guard spans per device, the few
//! columns of the job rows, and the quota snapshots of past windows.

use idlemax_core::report::{
    GuardSpan, GuardState, JobEvent, JobEventKind, JobFact, RETIRED_AFTER_MS,
};
use idlemax_core::types::{EventKind, JobSpec, QuotaSnapshot, UnixMs};
use rusqlite::{params, Connection, OptionalExtension};

use super::{conversion_err, enum_col, quota::snapshot_from_row, Store, StoreResult};

/// (id, end_ms, held, reasons) of a device's newest span, the only one a heartbeat can extend.
type Latest = (i64, UnixMs, bool, String);

impl Store {
    /// Spans that end at or after `since_ms`, plus each device's newest span that began
    /// before it. The second one is what lets a device that slept across `since_ms` (or
    /// through the whole period) be offline for the part of the period that it slept, not
    /// unknown. A device whose newest span ended more than `RETIRED_AFTER_MS` before the
    /// period is taken to be retired and is left out.
    ///
    /// The carried span is one indexed lookup per device (`guard_spans_by_start`), not a
    /// scan per row: this runs under the store lock that every heartbeat waits for.
    pub fn guard_spans_since(&self, since_ms: UnixMs) -> StoreResult<Vec<GuardSpan>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT device, start_ms, end_ms, held, reasons FROM guard_spans WHERE end_ms >= ?1
             UNION
             SELECT p.device, p.start_ms, p.end_ms, p.held, p.reasons
             FROM (SELECT DISTINCT device FROM guard_spans) AS d
             JOIN guard_spans AS p ON p.id = (
                  SELECT n.id FROM guard_spans AS n
                  WHERE n.device = d.device AND n.start_ms < ?1
                  ORDER BY n.start_ms DESC LIMIT 1)
             WHERE p.end_ms >= ?2
             ORDER BY 1, 2",
        )?;
        let rows = stmt.query_map(params![since_ms, since_ms - RETIRED_AFTER_MS], |r| {
            let (held, reasons): (bool, String) = (r.get(3)?, r.get(4)?);
            Ok(GuardSpan {
                device: r.get(0)?,
                start_ms: r.get(1)?,
                end_ms: r.get(2)?,
                state: GuardState::from_stored(held, &reasons),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Jobs that were unfinished at any time, that finished at or after `since_ms` by the
    /// agent's clock, or that the coordinator logged an event for since then, with the
    /// spec fields that decide which devices may take them. Only those fields are read out of
    /// the spec, which can hold a megabyte of stdin.
    pub fn job_facts_since(&self, since_ms: UnixMs) -> StoreResult<Vec<JobFact>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, status, device, created_ms, not_before_ms, started_ms, finished_ms,
                    (SELECT max(e.at_ms) FROM events AS e
                     WHERE e.job = jobs.id AND e.kind IN
                       ('job_succeeded', 'job_failed', 'job_killed', 'job_cancelled')),
                    json_extract(spec, '$.devices'), json_extract(spec, '$.requires'),
                    json_extract(spec, '$.cost'), ai_account, json_extract(spec, '$.command[0]')
             FROM jobs
             WHERE status IN ('queued', 'leased', 'running', 'paused')
                OR COALESCE(finished_ms, created_ms) >= ?1
                OR id IN (SELECT job FROM events WHERE job IS NOT NULL AND at_ms >= ?1)",
        )?;
        let rows = stmt.query_map([since_ms], |r| {
            let cost: Option<String> = r.get(10)?;
            Ok(JobFact {
                id: r.get(0)?,
                status: enum_col(r, 1)?,
                device: r.get(2)?,
                created_ms: r.get(3)?,
                not_before_ms: r.get(4)?,
                started_ms: r.get(5)?,
                finished_ms: r.get(6)?,
                outcome_at_ms: r.get(7)?,
                devices: json_list(r, 8)?,
                requires: json_list(r, 9)?,
                cost: cost.map_or_else(
                    || Ok(JobSpec::default().cost),
                    |c| serde_json::from_value(c.into()).map_err(|e| conversion_err(10, e)),
                )?,
                ai_account: r.get(11)?,
                program: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The start and end events of every job that had an event at or after `since_ms` or is
    /// not finished, in the order they happened. A job row keeps only its last attempt; these
    /// keep each one, on the coordinator's clock.
    pub fn job_events_since(&self, since_ms: UnixMs) -> StoreResult<Vec<JobEvent>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT job, kind, device, at_ms FROM events
             WHERE job IS NOT NULL
               AND kind IN ('job_started', 'job_succeeded', 'job_failed', 'job_killed',
                            'job_cancelled', 'job_parked', 'job_queued', 'lease_expired')
               AND job IN (SELECT job FROM events WHERE job IS NOT NULL AND at_ms >= ?1
                           UNION SELECT id FROM jobs
                                 WHERE status IN ('queued', 'leased', 'running', 'paused'))
             ORDER BY job, id",
        )?;
        let rows = stmt.query_map([since_ms], |r| {
            let kind: EventKind = enum_col(r, 1)?;
            Ok(JobEvent {
                job: r.get(0)?,
                device: r.get(2)?,
                at_ms: r.get(3)?,
                kind: if kind == EventKind::JobStarted {
                    JobEventKind::Started
                } else {
                    JobEventKind::Ended
                },
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Quota snapshots of every window cycle that resets at or after `since_ms`.
    pub fn quota_history_since(&self, since_ms: UnixMs) -> StoreResult<Vec<QuotaSnapshot>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT account, quota_window, used_pct, resets_at_ms, source, at_ms
             FROM quota_snapshots WHERE resets_at_ms >= ?1
             ORDER BY account, quota_window, at_ms",
        )?;
        let rows = stmt.query_map([since_ms], snapshot_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// A JSON array column as a list of strings; a missing one is an empty list.
fn json_list(r: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Vec<String>> {
    let text: Option<String> = r.get(idx)?;
    match text {
        None => Ok(Vec::new()),
        Some(t) => serde_json::from_str(&t).map_err(|e| conversion_err(idx, e)),
    }
}

/// Records the verdict a heartbeat carried: extends the device's newest span when the verdict
/// is the same and the device never went quiet, and starts a new span otherwise. A change
/// closes the old span at the moment of the change, so spans meet with no hole between them.
/// The caller owns the transaction.
pub(super) fn record_guard(
    conn: &Connection,
    device: &str,
    state: &GuardState,
    now: UnixMs,
    (offline_after_ms, max_spans): (i64, i64),
) -> StoreResult<()> {
    let latest: Option<Latest> = conn
        .prepare_cached(
            "SELECT id, end_ms, held, reasons FROM guard_spans
             WHERE device = ?1 ORDER BY id DESC LIMIT 1",
        )?
        .query_row([device], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let reasons = state.reasons_text();
    if let Some((id, end, held, old)) = latest {
        if now.saturating_sub(end) <= offline_after_ms {
            conn.prepare_cached("UPDATE guard_spans SET end_ms = max(end_ms, ?2) WHERE id = ?1")?
                .execute(params![id, now])?;
            if held == state.held && old == reasons {
                return Ok(());
            }
        }
    }
    conn.prepare_cached(
        "INSERT INTO guard_spans (device, start_ms, end_ms, held, reasons)
         VALUES (?1, ?2, ?2, ?3, ?4)",
    )?
    .execute(params![device, now, state.held, reasons])?;
    // A device that flaps keeps its newest `max_spans` and loses the oldest.
    conn.prepare_cached(
        "DELETE FROM guard_spans WHERE device = ?1 AND id <= (
           SELECT id FROM guard_spans WHERE device = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
    )?
    .execute(params![device, max_spans])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use idlemax_core::control::ControlPlan;
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::report::{HoldReason, PauseSpan};
    use idlemax_core::types::{
        ControlScope, DeviceInfo, EventKind, GuardAction, GuardDecision, JobSpec, JobStatus,
    };

    use super::super::Caps;
    use super::*;

    const T0: i64 = 1_800_000_000_000;
    const DAY_MS: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    const MIN: i64 = 60_000;

    fn beat(store: &mut Store, secs: i64, action: GuardAction, reasons: &[&str]) {
        let hb = Heartbeat {
            device: DeviceInfo {
                id: "mac".into(),
                ..DeviceInfo::default()
            },
            guard: GuardDecision {
                action,
                reasons: reasons.iter().map(|r| r.to_string()).collect(),
                ..GuardDecision::default()
            },
            ..Heartbeat::default()
        };
        store.record_heartbeat(&hb, T0 + secs * 1000).unwrap();
    }

    #[test]
    fn a_span_is_written_per_verdict_change_and_a_silent_gap_starts_a_new_one() {
        let mut store = Store::open_in_memory().unwrap();
        let run = GuardAction::Run {
            slots: 1,
            mem_mib: 1,
        };
        let why = ["pause: memory pressure Warn"];
        for secs in [0, 30, 60, 90] {
            beat(&mut store, secs, run, &[]);
        }
        beat(&mut store, 120, GuardAction::Pause, &why);
        beat(&mut store, 150, GuardAction::Pause, &why);
        // Ten silent minutes: the verdict is the same, but the gap is not part of the span.
        beat(&mut store, 750, GuardAction::Pause, &why);

        let spans = store.guard_spans_since(0).unwrap();
        let seen: Vec<(i64, i64, bool)> = spans
            .iter()
            .map(|s| (s.start_ms - T0, s.end_ms - T0, s.state.held))
            .collect();
        assert_eq!(
            seen,
            [
                (0, 120_000, false),
                (120_000, 150_000, true),
                (750_000, 750_000, true)
            ]
        );
        assert_eq!(spans[1].state.reasons, [HoldReason::Memory]);

        let pruned = store.prune(T0 + 31 * DAY_MS, 7 * DAY_MS).unwrap();
        assert_eq!(pruned.guard_spans, 3);
    }
    #[test]
    fn a_clock_step_back_keeps_writing_one_row_per_verdict_change() {
        let mut store = Store::open_in_memory().unwrap();
        let run = GuardAction::Run {
            slots: 1,
            mem_mib: 1,
        };
        let why = ["pause: memory pressure Warn"];
        beat(&mut store, 0, run, &[]);
        beat(&mut store, 30, run, &[]);
        // The coordinator's clock steps back an hour; the device now holds for the same reason
        // on every heartbeat. That is one new span, not one per heartbeat.
        for secs in [-3600, -3570, -3540, -3510, -3480] {
            beat(&mut store, secs, GuardAction::Pause, &why);
        }
        assert_eq!(store.guard_spans_since(-86_400).unwrap().len(), 2);
    }
    /// Writes `n` closed spans for each of `devices` straight into the table, one a minute.
    fn fill(store: &Store, devices: &[&str], n: i64) {
        let tx = store.conn.unchecked_transaction().unwrap();
        for device in devices {
            for i in 0..n {
                let start = T0 + i * 60_000;
                tx.execute(
                    "INSERT INTO guard_spans (device, start_ms, end_ms, held, reasons)
                     VALUES (?1, ?2, ?3, ?4, '')",
                    params![device, start, start + 30_000, i % 2],
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    #[test]
    fn the_query_stays_fast_on_ten_thousand_spans() {
        let store = Store::open_in_memory().unwrap();
        fill(&store, &["mac", "srv"], 5_000);
        // A day-long window in the middle of the history, as the dashboard asks for.
        let since = T0 + 2_000 * 60_000;
        let started = std::time::Instant::now();
        let spans = store.guard_spans_since(since).unwrap();
        let took = started.elapsed();
        // The window's spans, plus the newest span before it for each device.
        assert!(
            spans.len() > 2 * 3_000 && spans.len() < 10_000,
            "{}",
            spans.len()
        );
        assert!(took.as_millis() < 1_000, "took {took:?}");
    }
    fn insert_span(store: &Store, device: &str, start: i64, end: i64) {
        store
            .conn
            .execute(
                "INSERT INTO guard_spans (device, start_ms, end_ms, held, reasons)
                 VALUES (?1, ?2, ?3, 0, '')",
                params![device, start, end],
            )
            .unwrap();
    }

    #[test]
    fn a_flapping_device_keeps_only_its_newest_spans() {
        let caps = Caps {
            guard_spans: 4,
            ..Caps::default()
        };
        let mut store = Store::open_in_memory().unwrap().with_caps(caps);
        let run = GuardAction::Run {
            slots: 1,
            mem_mib: 1,
        };
        // The verdict flips on every heartbeat: ten changes, room for four.
        for i in 0..10 {
            if i % 2 == 0 {
                beat(&mut store, i * 30, run, &[]);
            } else {
                beat(
                    &mut store,
                    i * 30,
                    GuardAction::Pause,
                    &["pause: on battery"],
                );
            }
        }
        let spans = store.guard_spans_since(0).unwrap();
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[3].start_ms - T0, 9 * 30_000);
    }

    #[test]
    fn a_device_silent_for_over_a_week_before_the_period_is_left_out_of_it() {
        let store = Store::open_in_memory().unwrap();
        let since = T0 + 30 * DAY_MS;
        // The retired one ended 20 days before the period began, the napping one 3 days before.
        insert_span(&store, "retired", T0 + 9 * DAY_MS, T0 + 9 * DAY_MS + 30_000);
        insert_span(
            &store,
            "napping",
            T0 + 27 * DAY_MS,
            T0 + 27 * DAY_MS + 30_000,
        );
        let devices: Vec<String> = store
            .guard_spans_since(since)
            .unwrap()
            .into_iter()
            .map(|s| s.device)
            .collect();
        assert_eq!(devices, ["napping"]);
    }

    #[test]
    fn pauses_are_kept_as_spans_and_those_in_force_at_an_upgrade_start_then() {
        let plan = |scope, paused| ControlPlan {
            pause: Some((scope, paused)),
            jobs: Vec::new(),
            summary: String::new(),
        };
        let caps = Caps {
            pause_spans: 2,
            ..Caps::default()
        };
        let mut store = Store::open_in_memory().unwrap().with_caps(caps);
        let srv = ControlScope::Device("srv".into());
        store
            .apply_control(&plan(ControlScope::Global, true), T0)
            .unwrap();
        store
            .apply_control(&plan(ControlScope::Global, false), T0 + HOUR)
            .unwrap();
        store
            .apply_control(&plan(srv.clone(), true), T0 + 2 * HOUR)
            .unwrap();
        let pauses = store.pause_spans_since(T0).unwrap();
        let expected = [
            PauseSpan {
                scope: ControlScope::Global,
                from_ms: T0,
                to_ms: Some(T0 + HOUR),
            },
            PauseSpan {
                scope: srv.clone(),
                from_ms: T0 + 2 * HOUR,
                to_ms: None,
            },
        ];
        assert_eq!(pauses, expected);
        // Flapping a pause cannot grow the table: the oldest closed spans go first.
        for i in 3..8 {
            store
                .apply_control(&plan(ControlScope::Global, i % 2 == 1), T0 + i * HOUR)
                .unwrap();
        }
        assert!(store.pause_spans_since(0).unwrap().len() <= 3);

        // A pause already in force when this version first runs starts then.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for sql in &super::super::schema::MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute(
            "INSERT INTO controls (scope_type, scope_id) VALUES ('global', '')",
            [],
        )
        .unwrap();
        let upgraded = Store::init(conn, false).unwrap();
        let open = upgraded.pause_spans_since(0).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(
            (&open[0].scope, open[0].to_ms),
            (&ControlScope::Global, None)
        );
    }

    #[test]
    fn a_job_the_coordinator_saw_finish_in_the_period_is_read_though_the_agent_dated_it_before() {
        let mut store = Store::open_in_memory().unwrap();
        let spec = JobSpec {
            name: "j".into(),
            command: vec!["true".into()],
            ..JobSpec::default()
        };
        let mut job = store.insert_job(&spec, T0).unwrap();
        // The agent's clock is two minutes behind: it stamped the finish at minute 59.
        job.status = JobStatus::Succeeded;
        job.started_ms = Some(T0 + 50 * MIN);
        job.finished_ms = Some(T0 + 59 * MIN);
        store.update_jobs(&[job.clone()]).unwrap();
        let mac = "mac".to_string();
        let at = T0 + 61 * MIN;
        store
            .append_event(EventKind::JobSucceeded, Some(&mac), Some(job.id), "j", at)
            .unwrap();
        let facts = store.job_facts_since(T0 + 60 * MIN).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].outcome_at_ms, Some(at));
    }
}
