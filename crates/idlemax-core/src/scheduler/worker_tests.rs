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
