//! Tests: the one-line cluster summary names every kind of finished-badly job.

use idlemax_core::protocol::{ClusterStatus, JobCounts, LocalStatus};
use idlemax_core::types::{GuardAction, GuardDecision};

use super::{local_status, summary};

#[test]
fn keep_awake_is_a_separate_status_line_only_when_reported() {
    use crate::cli::out::{toon::encode, views};
    use idlemax_core::explain::{DeviceExplanation, ExplainState, Explanation};
    use idlemax_core::protocol::DeviceView;

    let note = "holding (queued work, on AC)";
    let mut status = ClusterStatus {
        devices: vec![DeviceView {
            keep_awake: Some(note.into()),
            info: idlemax_core::types::DeviceInfo {
                id: "mac".into(),
                ..Default::default()
            },
            guard: Some(GuardDecision::default()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut why = Explanation {
        job: 1,
        name: "waiting".into(),
        status: idlemax_core::types::JobStatus::Queued,
        state: ExplainState::Blocked,
        summary: "waiting".into(),
        reasons: vec![],
        at_ms: 0,
        devices: vec![DeviceExplanation {
            keep_awake: Some(note.into()),
            device: "mac".into(),
            online: true,
            last_seen_ms: 0,
            ready: false,
            ineligible: vec![],
            reasons: vec![],
        }],
    };
    assert!(super::status(&status, 0).contains(&format!("\nmac keep-awake: {note}")));
    assert!(crate::cli::why::render(&why).contains(&format!("\n  keep-awake: {note}")));
    assert!(encode(&views::status(&status, 0, &[]).unwrap()).contains(note));
    assert!(encode(&views::why(&why)).contains(note));
    assert_eq!(
        crate::cli::rows::guard_summary(status.devices[0].guard.as_ref().unwrap()),
        "hold"
    );
    status.devices[0].keep_awake = None;
    why.devices[0].keep_awake = None;
    assert!(!super::status(&status, 0).contains("keep-awake"));
    assert!(!crate::cli::why::render(&why).contains("keep-awake"));
    assert!(!encode(&views::status(&status, 0, &[]).unwrap()).contains("keep_awake"));
    assert!(!encode(&views::why(&why)).contains("keep_awake"));
    let old: LocalStatus = serde_json::from_str("{}").unwrap();
    assert!(old.keep_awake.is_none());
    assert!(serde_json::to_value(old)
        .unwrap()
        .get("keep_awake")
        .is_none());
}

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
