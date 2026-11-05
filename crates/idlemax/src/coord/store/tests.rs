//! Tests: store lease compare-and-set against real SQLite.

use idlemax_core::types::{CostClass, Job, JobSpec, JobStatus};

use super::{Store, StoreError};

const T0: i64 = 1_800_000_000_000;

#[test]
fn control_rolls_back_pause_when_job_write_fails() {
    use idlemax_core::types::ControlScope;
    let mut s = Store::open_in_memory().unwrap();
    let job = s.insert_job(&spec("control", None), T0).unwrap();
    let changed = Job {
        status: JobStatus::Paused,
        ..job.clone()
    };
    s.conn.execute_batch("CREATE TRIGGER fail_jobs BEFORE UPDATE ON jobs BEGIN SELECT RAISE(ABORT, 'job write failed'); END;").unwrap();
    let plan = idlemax_core::control::ControlPlan {
        pause: Some((ControlScope::Global, true)),
        jobs: vec![changed],
        summary: "pause".into(),
    };
    assert!(s.apply_control(&plan).is_err());
    assert!(s.paused_scopes().unwrap().is_empty());
    assert_eq!(s.get_job(job.id).unwrap(), Some(job));
}

#[test]
fn advice_rolls_back_job_when_status_write_fails() {
    use idlemax_core::types::{
        Recommendation, RecommendationKind, RecommendationSource, RecommendationStatus,
    };
    let mut s = Store::open_in_memory().unwrap();
    let job_spec = spec("advice", None);
    let id = s
        .insert_recommendation(&Recommendation {
            id: 0,
            at_ms: T0,
            key: "atomic".into(),
            kind: RecommendationKind::Assign,
            title: "test".into(),
            why: "test".into(),
            evidence: vec![],
            suggested_job: Some(job_spec.clone()),
            confidence: 1.0,
            source: RecommendationSource::Rules,
            status: RecommendationStatus::Open,
        })
        .unwrap();
    s.conn.execute_batch("CREATE TRIGGER fail_advice BEFORE UPDATE ON recommendations BEGIN SELECT RAISE(ABORT, 'advice write failed'); END;").unwrap();
    assert!(s.accept_advice(id, &job_spec, T0).is_err());
    assert!(s.list_jobs(None, 0).unwrap().is_empty());
    assert_eq!(
        s.get_recommendation(id).unwrap().unwrap().status,
        RecommendationStatus::Open
    );
}

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
    s.update_jobs(&[done]).unwrap();
    let backoff = Job {
        not_before_ms: Some(T0 + 60_000),
        ..b
    };
    s.update_jobs(&[backoff]).unwrap();
    assert_eq!(s.lease_job(b.id, "srv", T0 + 120_000, T0).unwrap(), None);
    assert!(s
        .lease_job(b.id, "srv", T0 + 180_000, T0 + 60_000)
        .unwrap()
        .is_some());
    assert_eq!(s.job_counts().unwrap().succeeded, 1);
    assert_eq!(s.job_counts().unwrap().leased, 1);
}
