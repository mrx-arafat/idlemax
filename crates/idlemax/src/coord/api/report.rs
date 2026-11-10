//! `GET /v1/report`. Adapter: reads the guard spans, job rows and quota history from the
//! store and asks the domain `report::build`; it decides nothing itself.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::Json;
use idlemax_core::report::{
    build, label_ok, runs_from_events, Inputs, Report, Window, MAX_DAYS, MAX_LABEL_CHARS,
};
use idlemax_core::types::{DeviceInfo, UnixMs};
use idlemax_core::Config;
use serde::Deserialize;

use super::http::ApiFailure;
use super::{offline_after_ms, AppState};
use crate::coord::store::{Store, StoreError};
use crate::state::now_ms;

const DAY_MS: i64 = 86_400_000;

/// `?days=7`, or `?since_ms=<unix ms>`; `label` names the period in the verdict line.
#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    days: Option<i64>,
    since_ms: Option<UnixMs>,
    label: Option<String>,
}

pub async fn report(
    State(app): State<AppState>,
    query: Result<Query<ReportQuery>, QueryRejection>,
) -> Result<Json<Report>, ApiFailure> {
    let Query(q) = query?;
    let window = window(&q, now_ms()).map_err(ApiFailure::BadRequest)?;
    let report = app
        .with_store(move |store, cfg| Ok::<_, ApiFailure>(report_for(store, cfg, &window)?))
        .await?;
    Ok(Json(report))
}

/// The period a query asks for, ending now. Neither `days` nor `since_ms` means one day.
fn window(q: &ReportQuery, now: UnixMs) -> Result<Window, String> {
    let (since_ms, default_label) = match (q.days, q.since_ms) {
        (Some(_), Some(_)) => return Err("give days or since_ms, not both".into()),
        (_, Some(since)) if since < now - MAX_DAYS * DAY_MS => {
            return Err(format!("since_ms must be within the last {MAX_DAYS} days"));
        }
        // A client whose clock runs ahead of this one just after midnight asks for a start in
        // the future: that is a period that has only begun.
        (_, Some(since)) => (since.min(now), "this period".to_string()),
        (Some(days), None) if !(1..=MAX_DAYS).contains(&days) => {
            return Err(format!("days must be 1 to {MAX_DAYS}"));
        }
        (days, None) => {
            let days = days.unwrap_or(1);
            let label = if days == 1 {
                "last 24 h".to_string()
            } else {
                format!("last {days} days")
            };
            (now - days * DAY_MS, label)
        }
    };
    let label = match &q.label {
        Some(l) if !label_ok(l) => {
            return Err(format!(
                "label must be 1 to {MAX_LABEL_CHARS} letters, digits, spaces, '.', '_' or '-'"
            ));
        }
        Some(l) => l.trim().to_string(),
        None => default_label,
    };
    Ok(Window {
        label,
        since_ms,
        until_ms: now,
    })
}

/// Reads what the report needs in one store call, so every number comes from the same instant.
fn report_for(store: &Store, cfg: &Config, window: &Window) -> Result<Report, StoreError> {
    let now = window.until_ms;
    let offline_after = offline_after_ms(cfg);
    let spans = store.guard_spans_since(window.since_ms)?;
    let jobs = store.job_facts_since(window.since_ms)?;
    let runs = runs_from_events(&store.job_events_since(window.since_ms)?);
    let devices: Vec<DeviceInfo> = store
        .devices(now, offline_after)?
        .into_iter()
        .map(|d| d.info)
        .collect();
    let pauses = store.pause_spans_since(window.since_ms)?;
    let quota = store.quota_history_since(window.since_ms)?;
    let retention_ms = i64::from(cfg.coordinator.metrics_retention_days).saturating_mul(DAY_MS);
    let input = Inputs {
        spans: &spans,
        jobs: &jobs,
        runs: &runs,
        pauses: &pauses,
        devices: &devices,
        quota: &quota,
        quota_kept_since_ms: now.saturating_sub(retention_ms),
        accounts_configured: !cfg.quota.accounts.is_empty(),
        offline_after_ms: offline_after,
    };
    Ok(build(window, &input))
}

#[cfg(test)]
mod tests {
    use idlemax_core::control::ControlPlan;
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::report::HoldReason;
    use idlemax_core::types::{
        ControlScope, DeviceInfo, EventKind, GuardAction, GuardDecision, JobSpec, JobStatus, UnixMs,
    };

    use super::*;

    const T0: i64 = 1_800_000_000_000;
    const MIN: i64 = 60_000;
    const HOUR: i64 = 60 * MIN;

    fn heartbeat(action: GuardAction, reasons: &[&str], at: UnixMs) -> Heartbeat {
        Heartbeat {
            device: DeviceInfo {
                id: "mac".into(),
                ..DeviceInfo::default()
            },
            guard: GuardDecision {
                action,
                reasons: reasons.iter().map(|r| r.to_string()).collect(),
                at_ms: at,
                ..GuardDecision::default()
            },
            ..Heartbeat::default()
        }
    }

    fn query(days: Option<i64>, since_ms: Option<i64>, label: Option<&str>) -> ReportQuery {
        ReportQuery {
            days,
            since_ms,
            label: label.map(str::to_string),
        }
    }

    #[test]
    fn heartbeats_become_guard_spans_and_the_report_says_what_held_the_machine() {
        let mut store = Store::open_in_memory().unwrap();
        let run = GuardAction::Run {
            slots: 2,
            mem_mib: 1024,
        };
        // One heartbeat every 30 s: an hour of room, then an hour of memory pressure.
        for i in 0..240 {
            let at = T0 + i * 30_000;
            let hb = if i < 120 {
                heartbeat(run, &[], at)
            } else {
                heartbeat(GuardAction::Pause, &["pause: memory pressure Warn"], at)
            };
            store.record_heartbeat(&hb, at).unwrap();
        }
        let spec = JobSpec {
            name: "sleep".into(),
            command: vec!["true".into()],
            ..JobSpec::default()
        };
        let mut job = store.insert_job(&spec, T0 + 10 * MIN).unwrap();
        job.status = JobStatus::Succeeded;
        job.device = Some("mac".into());
        job.started_ms = Some(T0 + 10 * MIN);
        job.finished_ms = Some(T0 + 14 * MIN);
        store.update_jobs(&[job]).unwrap();

        let w = Window {
            label: "today".into(),
            since_ms: T0,
            until_ms: T0 + 240 * 30_000,
        };
        let r = report_for(&store, &Config::default(), &w).unwrap();
        assert_eq!(
            r.verdict,
            "today: 1 job ran (4 job-min); guard held 1.0 h, all of it by memory pressure; \
             queue was empty 56 min of idle time"
        );
    }

    #[test]
    fn a_retried_jobs_attempts_come_from_its_events_not_its_row() {
        let mut store = Store::open_in_memory().unwrap();
        let run = GuardAction::Run {
            slots: 2,
            mem_mib: 1024,
        };
        for i in 0..=120 {
            let at = T0 + i * 30_000;
            store
                .record_heartbeat(&heartbeat(run, &[], at), at)
                .unwrap();
        }
        let spec = JobSpec {
            name: "flaky".into(),
            command: vec!["true".into()],
            ..JobSpec::default()
        };
        let mut job = store.insert_job(&spec, T0).unwrap();
        // Two attempts: 10 to 14 minutes (put back in the queue) and 20 to 24. Leasing the
        // job again cleared the first start, so the row only knows the second.
        let mac = "mac".to_string();
        for (kind, minute) in [
            (EventKind::JobStarted, 10),
            (EventKind::JobQueued, 14),
            (EventKind::JobStarted, 20),
            (EventKind::JobSucceeded, 24),
        ] {
            store
                .append_event(kind, Some(&mac), Some(job.id), "flaky", T0 + minute * MIN)
                .unwrap();
        }
        job.status = JobStatus::Succeeded;
        job.device = Some(mac);
        job.attempts = 2;
        job.started_ms = Some(T0 + 20 * MIN);
        job.finished_ms = Some(T0 + 24 * MIN);
        store.update_jobs(&[job]).unwrap();

        let w = Window {
            label: "today".into(),
            since_ms: T0,
            until_ms: T0 + 60 * MIN,
        };
        let r = report_for(&store, &Config::default(), &w).unwrap();
        assert_eq!((r.jobs.ran, r.jobs.job_ms), (1, 8 * MIN));
    }

    #[test]
    fn a_pause_the_owner_set_reads_as_a_hold_and_not_as_work_waiting() {
        let mut store = Store::open_in_memory().unwrap();
        let run = GuardAction::Run {
            slots: 2,
            mem_mib: 1024,
        };
        for i in 0..=120 {
            let at = T0 + i * 30_000;
            store
                .record_heartbeat(&heartbeat(run, &[], at), at)
                .unwrap();
        }
        let spec = JobSpec {
            name: "waits".into(),
            command: vec!["true".into()],
            ..JobSpec::default()
        };
        store.insert_job(&spec, T0).unwrap();
        // Everything paused from minute 10 to minute 40.
        for (paused, minute) in [(true, 10), (false, 40)] {
            let plan = ControlPlan {
                pause: Some((ControlScope::Global, paused)),
                jobs: Vec::new(),
                summary: String::new(),
            };
            store.apply_control(&plan, T0 + minute * MIN).unwrap();
        }
        let w = Window {
            label: "today".into(),
            since_ms: T0,
            until_ms: T0 + 60 * MIN,
        };
        let r = report_for(&store, &Config::default(), &w).unwrap();
        assert_eq!(r.guard.held_ms, 30 * MIN);
        assert_eq!(r.guard.held_by[0].reason, HoldReason::Paused);
        // The job waited 2 to 10 and 40 to 60 with a free device; the pause is not waiting.
        assert_eq!(r.idle.work_waiting_ms, 28 * MIN);
    }

    #[test]
    fn a_sleep_that_began_before_the_window_is_offline_in_it() {
        let mut store = Store::open_in_memory().unwrap();
        let run = GuardAction::Run {
            slots: 2,
            mem_mib: 1024,
        };
        // Awake for half a minute, asleep for ten hours, awake again for half a minute.
        for at in [T0, T0 + 30_000, T0 + 10 * HOUR, T0 + 10 * HOUR + 30_000] {
            store
                .record_heartbeat(&heartbeat(run, &[], at), at)
                .unwrap();
        }
        // The report starts five hours into the sleep.
        let w = Window {
            label: "today".into(),
            since_ms: T0 + 5 * HOUR,
            until_ms: T0 + 10 * HOUR + 60_000,
        };
        let r = report_for(&store, &Config::default(), &w).unwrap();
        assert_eq!(r.guard.offline_ms, 5 * HOUR);
        assert_eq!(r.guard.tracked_ms, 5 * HOUR + 60_000);
    }

    #[test]
    fn a_query_names_one_period_and_a_safe_label() {
        let now = T0 + 40 * DAY_MS;
        let ok = window(&query(None, None, None), now).unwrap();
        assert_eq!(
            (ok.label.as_str(), ok.since_ms),
            ("last 24 h", now - DAY_MS)
        );
        let week = window(&query(Some(7), None, Some("last 7 days")), now).unwrap();
        assert_eq!(
            (week.label.as_str(), week.since_ms),
            ("last 7 days", now - 7 * DAY_MS)
        );
        // A client clock a few seconds ahead of this one is a period that has just begun, not
        // a bad request.
        for ahead in [now, now + 5_000] {
            let early = window(&query(None, Some(ahead), Some("today")), now).unwrap();
            assert_eq!((early.since_ms, early.until_ms), (now, now));
        }
        for bad in [
            query(Some(0), None, None),
            query(Some(31), None, None),
            query(Some(1), Some(now - MIN), None),
            query(None, Some(now - 31 * DAY_MS), None),
            query(Some(1), None, Some("bad\nlabel")),
        ] {
            assert!(window(&bad, now).is_err());
        }
    }
}
