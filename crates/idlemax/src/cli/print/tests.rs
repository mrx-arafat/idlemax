//! Tests: the one-line cluster summary names every kind of finished-badly job.

use idlemax_core::protocol::{ClusterStatus, JobCounts, LocalStatus};
use idlemax_core::types::{GuardAction, GuardDecision};

use super::{local_status, summary};

#[test]
fn local_status_prints_the_control_result_before_the_state() {
    let s = LocalStatus {
        device: "mac".into(),
        version: "0.2.0".into(),
        coordinator_reachable: true,
        ..LocalStatus::default()
    };
    assert!(local_status(Some("resumed this device"), &s, 0)
        .starts_with("ok: resumed this device\ndevice mac agent 0.2.0\n"));
    assert!(local_status(None, &s, 0).starts_with("device mac agent 0.2.0\n"));
}

#[test]
fn local_status_lists_every_guard_reason_once_whichever_the_summary_picks() {
    let s = LocalStatus {
        guard: GuardDecision {
            action: GuardAction::Kill,
            reasons: vec![
                "pause: memory pressure Warn".into(),
                "kill: disk free 512 MiB < 1024 MiB".into(),
            ],
            ..GuardDecision::default()
        },
        ..LocalStatus::default()
    };
    let out = local_status(None, &s, 0);
    for reason in &s.guard.reasons {
        assert_eq!(out.matches(&format!("\n  {reason}")).count(), 1, "{out}");
    }
}

fn line(jobs: JobCounts) -> String {
    summary(&ClusterStatus {
        jobs,
        ..ClusterStatus::default()
    })
}

#[test]
fn the_summary_counts_failed_killed_and_parked_jobs_when_there_are_any() {
    let busy = line(JobCounts {
        queued: 2,
        parked: 1,
        failed: 3,
        killed: 4,
        ..JobCounts::default()
    });
    assert!(
        busy.contains("jobs: 0 running, 2 queued, 1 parked, 3 failed, 4 killed |"),
        "{busy}"
    );
    let quiet = line(JobCounts::default());
    assert!(quiet.contains("jobs: 0 running, 0 queued |"), "{quiet}");
}
