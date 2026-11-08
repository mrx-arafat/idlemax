//! Tests: recovering a running job after lease expiry without replacing its attempt.

use super::*;
use crate::protocol::JobUpdate;
use crate::scheduler::{apply_update, expire_leases, pick, SchedContext};
use crate::types::{DeviceInfo, GuardAction, GuardDecision, JobSpec};

fn expired() -> Job {
    let job = Job {
        id: 1,
        status: JobStatus::Running,
        device: Some("mac".into()),
        attempts: 1,
        lease_expires_ms: Some(500),
        spec: JobSpec {
            max_attempts: 3,
            ..JobSpec::default()
        },
        ..Job::default()
    };
    expire_leases(&[job], 1_000).remove(0)
}

#[test]
fn waking_agent_recovers_its_unreplaced_expired_run() {
    let job = expired();
    assert_eq!(job.status, JobStatus::Queued);
    for status in [JobStatus::Running, JobStatus::Paused] {
        let reported = RunningJob {
            id: job.id,
            status,
            started_ms: 100,
            ..RunningJob::default()
        };
        let plan = reconcile_heartbeat("mac", &[reported], std::slice::from_ref(&job), &[], 2_000);
        assert!(!plan
            .commands
            .contains(&AgentCommand::KillJob { id: job.id }));
        let renewed = &plan.renewed[0];
        assert_eq!(renewed.device.as_deref(), Some("mac"));
        assert_eq!(renewed.status, status);
        assert_eq!(renewed.attempts, 1);
        assert_eq!(renewed.not_before_ms, None);
        assert!(renewed.lease_expires_ms > Some(2_000));
    }
}

#[test]
fn previous_attempt_cannot_overwrite_a_new_lease_on_the_same_device() {
    let mut job = expired();
    job.status = JobStatus::Leased;
    job.device = Some("mac".into());
    job.attempts = 2;
    let update = JobUpdate {
        id: job.id,
        device: "mac".into(),
        attempt: 1,
        status: JobStatus::Killed,
        ..JobUpdate::default()
    };
    assert_eq!(apply_update(&job, &update, 2_000), None);
}

#[test]
fn reported_run_cannot_be_leased_again_before_recovery() {
    let job = expired();
    let device = DeviceInfo {
        id: "mac".into(),
        ..DeviceInfo::default()
    };
    let guard = GuardDecision {
        action: GuardAction::Run {
            slots: 2,
            mem_mib: 4096,
        },
        ..GuardDecision::default()
    };
    let reported = [RunningJob {
        id: job.id,
        ..RunningJob::default()
    }];
    let ctx = SchedContext {
        now_ms: 200_000,
        device: &device,
        guard: &guard,
        running: &reported,
        quota: &[],
        paused: false,
        paused_accounts: &[],
    };
    assert_eq!(pick(&ctx, &[job]), Vec::<i64>::new());
}
