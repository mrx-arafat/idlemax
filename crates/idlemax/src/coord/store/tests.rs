//! Tests: store lease compare-and-set against real SQLite.

use idlemax_core::types::{CostClass, Job, JobSpec, JobStatus};

use super::{Store, StoreError};

const T0: i64 = 1_800_000_000_000;

fn spec(name: &str, ai_account: Option<&str>) -> JobSpec {
    JobSpec {
        name: name.into(),
        command: vec!["true".into()],
        cost: if ai_account.is_some() {
            CostClass::Ai
        } else {
            CostClass::CpuLight
        },
        ai_account: ai_account.map(Into::into),
        ..JobSpec::default()
    }
}

#[test]
fn lease_is_compare_and_set_with_one_ai_lease_per_account() {
    let mut s = Store::open_in_memory().unwrap();
    let a = s.insert_job(&spec("a", Some("claude-max")), T0).unwrap();
    let b = s.insert_job(&spec("b", Some("claude-max")), T0).unwrap();

    let leased = s.lease_job(a.id, "mac", T0 + 120_000, T0).unwrap().unwrap();
    assert_eq!((leased.status, leased.attempts), (JobStatus::Leased, 1));
    assert_eq!(leased.device.as_deref(), Some("mac"));
    assert_eq!(s.lease_job(a.id, "srv", T0 + 120_000, T0).unwrap(), None);
    assert!(matches!(
        s.lease_job(b.id, "srv", T0 + 120_000, T0),
        Err(StoreError::AccountLeased(acct)) if acct == "claude-max"
    ));
    assert_eq!(
        s.ai_leased_accounts().unwrap(),
        vec!["claude-max".to_string()]
    );

    let done = Job {
        status: JobStatus::Succeeded,
        ..leased
    };
    s.update_job(&done).unwrap();
    let backoff = Job {
        not_before_ms: Some(T0 + 60_000),
        ..b
    };
    s.update_job(&backoff).unwrap();
    assert_eq!(s.lease_job(b.id, "srv", T0 + 120_000, T0).unwrap(), None);
    assert!(s
        .lease_job(b.id, "srv", T0 + 180_000, T0 + 60_000)
        .unwrap()
        .is_some());
    assert_eq!(s.job_counts().unwrap().succeeded, 1);
    assert_eq!(s.job_counts().unwrap().leased, 1);
}
