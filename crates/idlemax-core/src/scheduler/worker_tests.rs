//! Tests: worker jobs stand outside the one-AI-lease rule in both directions (A8).

use super::*;
use crate::quota::QuotaView;
use crate::types::{
    CostClass, DeviceInfo, GuardAction, GuardDecision, Job, JobId, JobSpec, JobStatus,
};
use crate::worker::WorkerSpec;

fn ai_job(id: JobId, worker: bool) -> Job {
    Job {
        id,
        spec: JobSpec {
            name: format!("j{id}"),
            cost: CostClass::Ai,
            ai_account: Some("claude-max".into()),
            worker: worker.then(WorkerSpec::default),
            ..Default::default()
        },
        created_ms: id,
        ..Default::default()
    }
}

#[test]
fn a_worker_job_never_holds_the_account_for_a_plain_ai_job() {
    let mut worker = ai_job(7, true);
    worker.status = JobStatus::Running;
    worker.device = Some("mac".into());
    let jobs = vec![ai_job(1, false), worker];
    assert!(ai_leased_accounts(&jobs).is_empty());

    let device = DeviceInfo {
        id: "mac".into(),
        ai_accounts: vec!["claude-max".into()],
        ..Default::default()
    };
    let guard = GuardDecision {
        action: GuardAction::Run {
            slots: 2,
            mem_mib: 4096,
        },
        ..Default::default()
    };
    let quota = vec![QuotaView {
        account: "claude-max".into(),
        headroom_pct: 50.0,
        spendable: true,
        ..Default::default()
    }];
    let ctx = SchedContext {
        now_ms: 1_000,
        device: &device,
        guard: &guard,
        running: &[],
        quota: &quota,
        paused: false,
        paused_accounts: &[],
    };
    assert_eq!(pick(&ctx, &jobs), vec![1]);
}

fn running_worker() -> Job {
    let mut j = ai_job(1, true);
    j.status = JobStatus::Running;
    j.device = Some("mac".into());
    j.attempts = 1;
    j.lease_expires_ms = Some(500);
    j
}

fn update(status: JobStatus, retryable: bool) -> crate::protocol::JobUpdate {
    crate::protocol::JobUpdate {
        id: 1,
        device: "mac".into(),
        attempt: 1,
        status,
        at_ms: 900,
        retryable,
        error: Some("why".into()),
        ..Default::default()
    }
}

#[test]
fn a_worker_job_ends_where_a_plain_job_would_be_queued_again() {
    use JobStatus::{Cancelled, Failed, Killed, Queued};
    let cases = [
        (update(Queued, false), Cancelled),
        (update(Killed, true), Killed),
        (update(Failed, true), Killed),
    ];
    for (u, want) in cases {
        let next = apply_update(&running_worker(), &u, 900).unwrap();
        assert_eq!(next.status, want, "{:?}", u.status);
        assert_eq!(next.finished_ms, Some(900));
        assert_eq!(next.lease_expires_ms, None);

        let mut plain = running_worker();
        plain.spec.worker = None;
        plain.spec.max_attempts = 3;
        let plain = apply_update(&plain, &u, 900).unwrap();
        assert!(
            matches!(plain.status, Queued | JobStatus::Parked),
            "{:?}",
            u.status
        );
    }
}

#[test]
fn an_expired_worker_lease_ends_the_job() {
    let mut leased = running_worker();
    leased.status = JobStatus::Leased;
    for job in [running_worker(), leased] {
        let expired = expire_leases(&[job], 600);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].status, JobStatus::Killed);
        assert_eq!(expired[0].finished_ms, Some(600));
        assert!(expired[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("lease expired"));
    }
}

#[test]
fn after_a_sleep_an_expired_worker_lease_is_renewed_and_a_plain_one_is_not() {
    let mut plain = running_worker();
    plain.id = 2;
    plain.spec.worker = None;
    let renewed = renew_worker_leases(&[running_worker(), plain], 10, 600);
    assert_eq!(renewed.len(), 1);
    assert_eq!(
        (
            renewed[0].id,
            renewed[0].status,
            renewed[0].lease_expires_ms
        ),
        (1, JobStatus::Running, Some(10_600))
    );
}
