//! Tests: interval syntax and the due decision.

use super::*;
use crate::types::JobStatus;

const T0: UnixMs = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

fn every(text: &str) -> Every {
    text.parse().unwrap()
}

fn schedule(every_text: &str, last_fired_ms: Option<UnixMs>) -> Schedule {
    Schedule {
        id: 1,
        spec: JobSpec::default(),
        every: every(every_text),
        paused: false,
        created_ms: T0,
        last_fired_ms,
        last_job: last_fired_ms.map(|_| 7),
    }
}

fn job(status: JobStatus, finished_ms: Option<UnixMs>) -> Job {
    Job {
        id: 7,
        status,
        finished_ms,
        ..Job::default()
    }
}

#[test]
fn intervals_parse_display_and_reject_with_a_next_step() {
    for (text, secs) in [
        ("1m", 60),
        ("90m", 5400),
        ("6h", 21_600),
        ("1d", 86_400),
        ("90s", 90),
    ] {
        let e = every(text);
        assert_eq!(e.secs(), secs, "{text}");
        assert_eq!(e.to_string(), text, "{text} displays as itself");
    }
    assert_eq!(
        every(" 24h ").to_string(),
        "1d",
        "a whole number of days reads as days"
    );
    for bad in ["", "5", "d", "1.5h", "-1d", "2w", "1 d", "1day"] {
        let err = bad.parse::<Every>().unwrap_err().to_string();
        assert!(err.contains("for example 90m, 6h or 1d"), "{bad:?}: {err}");
    }
    assert_eq!(
        "30s".parse::<Every>().unwrap_err(),
        EveryError::TooShort("30s".into())
    );
    assert_eq!(
        "0d".parse::<Every>().unwrap_err(),
        EveryError::TooShort("0d".into())
    );
    assert_eq!(
        "366d".parse::<Every>().unwrap_err(),
        EveryError::TooLong("366d".into())
    );
    assert!("99999999999999999999d".parse::<Every>().is_err());
    assert_eq!(serde_json::to_string(&every("6h")).unwrap(), "\"6h\"");
    assert_eq!(
        serde_json::from_str::<Every>("\"1d\"").unwrap(),
        every("1d")
    );
    assert!(serde_json::from_str::<Every>("\"soon\"").is_err());
}

#[test]
fn a_new_schedule_fires_at_once() {
    assert_eq!(decide(&schedule("1d", None), None, T0), Decision::Fire);
}

#[test]
fn it_waits_out_the_interval_then_fires() {
    let s = schedule("1d", Some(T0));
    let done = job(JobStatus::Succeeded, Some(T0 + HOUR));
    // The interval counts from the end of the last run, not from when it was queued.
    let due = T0 + HOUR + DAY;
    assert_eq!(
        decide(&s, Some(&done), T0 + DAY),
        Decision::Wait { until_ms: due }
    );
    assert_eq!(
        decide(&s, Some(&done), due - 1),
        Decision::Wait { until_ms: due }
    );
    assert_eq!(decide(&s, Some(&done), due), Decision::Fire);
}

#[test]
fn it_never_fires_while_the_last_job_is_still_active() {
    let s = schedule("1h", Some(T0));
    for status in [
        JobStatus::Queued,
        JobStatus::Leased,
        JobStatus::Running,
        JobStatus::Paused,
    ] {
        let active = job(status, None);
        assert_eq!(
            decide(&s, Some(&active), T0 + 30 * DAY),
            Decision::Running { job: 7, status },
            "{status}"
        );
    }
}

#[test]
fn every_end_state_lets_the_next_run_through() {
    let s = schedule("1h", Some(T0));
    for status in [
        JobStatus::Succeeded,
        JobStatus::Failed,
        JobStatus::Killed,
        JobStatus::Cancelled,
        JobStatus::Parked,
    ] {
        let ended = job(status, Some(T0 + 1000));
        assert_eq!(
            decide(&s, Some(&ended), T0 + 2 * HOUR),
            Decision::Fire,
            "{status}"
        );
    }
}

#[test]
fn downtime_queues_one_job_not_one_per_missed_period() {
    let mut s = schedule("1d", Some(T0));
    let done = job(JobStatus::Succeeded, Some(T0 + HOUR));
    let now = T0 + 10 * DAY;
    assert_eq!(decide(&s, Some(&done), now), Decision::Fire);

    // The coordinator records the fire; the next sweep sees an active job and waits.
    s.last_fired_ms = Some(now);
    let queued = job(JobStatus::Queued, None);
    assert_eq!(
        decide(&s, Some(&queued), now),
        Decision::Running {
            job: 7,
            status: JobStatus::Queued
        }
    );
    let ran = job(JobStatus::Succeeded, Some(now + HOUR));
    assert_eq!(
        decide(&s, Some(&ran), now + 2 * HOUR),
        Decision::Wait {
            until_ms: now + HOUR + DAY
        }
    );
}

#[test]
fn a_paused_schedule_holds_but_still_reports_its_active_job() {
    let mut s = schedule("1h", Some(T0));
    s.paused = true;
    let ended = job(JobStatus::Succeeded, Some(T0 + 1000));
    assert_eq!(decide(&s, Some(&ended), T0 + 5 * DAY), Decision::Paused);
    let active = job(JobStatus::Running, None);
    assert_eq!(
        decide(&s, Some(&active), T0 + 5 * DAY),
        Decision::Running {
            job: 7,
            status: JobStatus::Running
        }
    );
}

#[test]
fn a_clock_set_back_waits_one_interval_not_until_the_old_future() {
    let s = schedule("1d", Some(T0 + 400 * DAY));
    let now = T0;
    assert_eq!(
        decide(&s, None, now),
        Decision::Wait {
            until_ms: now + DAY
        }
    );
}
