//! Tests: polling stays polite and the exit code mirrors the job.

use std::time::Duration;

use super::*;

#[test]
fn polls_back_off_to_a_slow_steady_pace_without_overflow() {
    let secs = |n| poll_delay(n).as_secs_f64();
    assert_eq!([secs(0), secs(1), secs(2), secs(3)], [0.5, 1.0, 2.0, 4.0]);
    assert_eq!(poll_delay(4), Duration::from_secs(5));
    assert_eq!(poll_delay(10_000), Duration::from_secs(5));
    assert_eq!(poll_delay(u32::MAX), Duration::from_secs(5));
}

#[test]
fn only_a_succeeded_job_exits_zero() {
    for status in JobStatus::ALL.into_iter().filter(|s| s.is_terminal()) {
        let want = u8::from(status != JobStatus::Succeeded);
        assert_eq!(exit_code(status), want, "{status}");
    }
    assert_eq!(TIMED_OUT_EXIT, 124);
}
