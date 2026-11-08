//! Tests: what `job add --wait` prints for a finished, failed and abandoned job.

use idlemax_core::types::{JobSpec, JobStatus};

use super::*;

fn job(status: JobStatus, exit_code: Option<i32>) -> Job {
    Job {
        id: 12,
        spec: JobSpec {
            name: "echo".into(),
            ..JobSpec::default()
        },
        status,
        device: Some("mac".into()),
        started_ms: Some(1_000),
        finished_ms: Some(1_400),
        exit_code,
        ..Job::default()
    }
}

#[test]
fn a_result_shows_status_exit_code_run_time_and_the_log_end() {
    let log = Log::Lines(vec![
        "hi".into(),
        "=== idlemax: succeeded (exit 0) ===".into(),
    ]);
    assert_eq!(
        result_text(&job(JobStatus::Succeeded, Some(0)), &log),
        "job 12 succeeded: echo\nexit code: 0\nran: 0.4s on mac\n\
         log (last 20 lines):\nhi\n=== idlemax: succeeded (exit 0) ==="
    );

    let failed = Job {
        last_error: Some("exit code 3".into()),
        finished_ms: Some(125_000),
        ..job(JobStatus::Failed, Some(3))
    };
    let text = result_text(&failed, &Log::On("srv".into()));
    // "exit code 3" would only repeat the line above it.
    assert!(
        text.contains("exit code: 3\nran: 2m on mac\nlog: on device srv"),
        "{text}"
    );
    assert!(
        text.ends_with("log: on device srv; run `idlemax logs 12` there"),
        "{text}"
    );

    let capped = Job {
        last_error: Some("memory 900 MiB over the 512 MiB cap".into()),
        ..job(JobStatus::Killed, None)
    };
    let text = result_text(&capped, &Log::None);
    assert!(
        text.contains("ran: 0.4s on mac\nerror: memory 900 MiB over the 512 MiB cap\nlog: none"),
        "{text}"
    );

    let retried = Job {
        last_error: Some("exit code 3; attempt 2 of 3 next".into()),
        ..job(JobStatus::Queued, Some(3))
    };
    assert_eq!(
        progress_text(&retried),
        "job 12 queued again: exit code 3; attempt 2 of 3 next"
    );
    assert_eq!(
        progress_text(&job(JobStatus::Running, None)),
        "job 12 running"
    );

    let never = Job {
        started_ms: None,
        device: None,
        ..job(JobStatus::Cancelled, None)
    };
    let text = result_text(&never, &Log::None);
    assert!(
        text.contains("exit code: none\nran: never started"),
        "{text}"
    );
}

#[test]
fn a_timeout_says_the_job_is_still_queued_and_what_to_do() {
    let text = timeout_text(&job(JobStatus::Queued, None), Duration::from_millis(30_400));
    assert_eq!(
        text,
        "job 12 is still queued after waiting 30s: echo\n\
         it keeps its place: `idlemax why 12` shows what it waits for, \
         `idlemax job cancel 12` drops it"
    );
}
