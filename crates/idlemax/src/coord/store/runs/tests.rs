//! Tests: Run rows, leased worker jobs and the AI-rule exemption in the store.

use idlemax_core::types::{CostClass, JobStatus};
use idlemax_core::worker::WorkerSpec;

use super::super::schema;
use super::*;

const T0: UnixMs = 1_700_000_000_000;

fn ai_spec(name: &str, worker: bool) -> JobSpec {
    JobSpec {
        name: name.into(),
        command: vec!["idlemax-worker".into(), "claude".into()],
        cost: CostClass::Ai,
        ai_account: Some("max".into()),
        worker: worker.then(WorkerSpec::default),
        ..Default::default()
    }
}

fn run(project: &str, state: RunState) -> Run {
    Run {
        device: "mac".into(),
        project: project.into(),
        task: "roadmap:1".into(),
        account: "max".into(),
        model: "sonnet".into(),
        reason: "the oldest ready task".into(),
        state,
        start_ms: T0,
        ..Default::default()
    }
}

#[test]
fn a_v8_store_migrates_and_keeps_its_jobs_as_plain_ones() {
    let conn = Connection::open_in_memory().unwrap();
    for sql in &schema::MIGRATIONS[..8] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", 8).unwrap();
    conn.execute(
        "INSERT INTO jobs (spec, name, status, priority, created_ms)
         VALUES ('{\"name\":\"old\",\"command\":[\"true\"]}', 'old', 'queued', 0, ?1)",
        [T0],
    )
    .unwrap();
    let s = Store::init(conn, false).unwrap();
    let version: i64 = s
        .conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, schema::MIGRATIONS.len() as i64);
    assert_eq!(s.get_job(1).unwrap().unwrap().spec.name, "old");
    let worker: i64 = s
        .conn
        .query_row("SELECT worker FROM jobs WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(worker, 0);
}

#[test]
fn worker_jobs_stand_outside_the_one_ai_lease_rule() {
    let mut s = Store::open_in_memory().unwrap();
    for name in ["w1", "w2"] {
        let added = s
            .insert_leased_job(&ai_spec(name, true), "mac", T0 + 60_000, T0)
            .unwrap();
        assert_eq!(added.into_job().status, JobStatus::Leased);
    }
    let plain = s.insert_job(&ai_spec("p1", false), T0).unwrap();
    assert!(s
        .lease_job(plain.id, "mac", T0 + 60_000, T0)
        .unwrap()
        .is_some());
    let second = s.insert_job(&ai_spec("p2", false), T0).unwrap();
    assert!(matches!(
        s.lease_job(second.id, "mac", T0 + 60_000, T0),
        Err(StoreError::AccountLeased(_))
    ));
    assert_eq!(s.ai_leased_accounts().unwrap(), ["max"]);
}

#[test]
fn a_started_worker_job_is_not_an_ai_job_start() {
    let mut s = Store::open_in_memory().unwrap();
    let mut job = s
        .insert_leased_job(&ai_spec("w", true), "mac", T0 + 60_000, T0)
        .unwrap()
        .into_job();
    job.status = JobStatus::Running;
    job.started_ms = Some(T0);
    s.update_jobs(&[job]).unwrap();
    assert!(s.ai_job_starts(T0 - 1).unwrap().is_empty());
    assert!(s.ai_leased_accounts().unwrap().is_empty());
}

#[test]
fn a_live_key_returns_its_job_and_inserts_nothing() {
    let mut s = Store::open_in_memory().unwrap();
    let spec = JobSpec {
        key: Some("run-1".into()),
        ..ai_spec("w", true)
    };
    let first = s
        .insert_leased_job(&spec, "mac", T0 + 60_000, T0)
        .unwrap()
        .into_job();
    let again = s.insert_leased_job(&spec, "mac", T0 + 60_000, T0).unwrap();
    assert_eq!(again, Added::Existing(first));
    assert_eq!(s.list_jobs(None, 0).unwrap().len(), 1);
}

#[test]
fn update_run_writes_only_while_the_stored_state_matches() {
    let mut s = Store::open_in_memory().unwrap();
    let stored = s
        .insert_run(&run("demo", RunState::Starting), &RunLaunch::default())
        .unwrap();
    let mut next = stored.clone();
    next.state = RunState::Running;
    next.session = Some("abc".into());
    assert!(s.update_run(&next, RunState::Starting).unwrap());
    assert_eq!(s.get_run(stored.id).unwrap().unwrap(), next);

    let mut late = next.clone();
    late.state = RunState::Checking;
    assert!(!s.update_run(&late, RunState::Starting).unwrap());
    assert_eq!(s.get_run(stored.id).unwrap().unwrap(), next);
}

#[test]
fn runs_are_listed_by_state_and_project_newest_first() {
    let mut s = Store::open_in_memory().unwrap();
    let launch = RunLaunch::default();
    let a = s
        .insert_run(&run("a", RunState::Starting), &launch)
        .unwrap();
    let b = s
        .insert_run(&run("b", RunState::Starting), &launch)
        .unwrap();
    let c = s.insert_run(&run("a", RunState::Shed), &launch).unwrap();
    let ids = |runs: Vec<Run>| runs.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(ids(s.list_runs(None, None, 0).unwrap()), [c.id, b.id, a.id]);
    assert_eq!(ids(s.list_runs(None, Some("a"), 0).unwrap()), [c.id, a.id]);
    assert_eq!(
        ids(s.list_runs(Some(RunState::Starting), Some("a"), 0).unwrap()),
        [a.id]
    );
    assert_eq!(ids(s.list_runs(None, None, 1).unwrap()), [c.id]);
}

#[test]
fn insert_run_stores_every_field_it_is_given() {
    let mut s = Store::open_in_memory().unwrap();
    let mut given = run("demo", RunState::Starting);
    given.session = Some("sess".into());
    given.updated_ms = T0 + 5;
    given.launches = 1;
    let stored = s.insert_run(&given, &RunLaunch::default()).unwrap();
    given.id = stored.id;
    assert_eq!(stored, given);
}

#[test]
fn leased_jobs_count_against_the_live_jobs_cap() {
    let caps = super::super::Caps {
        live_jobs: 1,
        ..Default::default()
    };
    let mut s = Store::open_in_memory().unwrap().with_caps(caps);
    s.insert_leased_job(&ai_spec("w1", true), "mac", T0 + 1, T0)
        .unwrap();
    assert!(matches!(
        s.insert_leased_job(&ai_spec("w2", true), "mac", T0 + 1, T0),
        Err(StoreError::LimitReached { .. })
    ));
}

#[test]
fn prune_drops_old_kept_and_discarded_runs_even_without_an_end_time() {
    let mut s = Store::open_in_memory().unwrap();
    let launch = RunLaunch::default();
    let mut kept = run("a", RunState::Kept);
    kept.end_ms = Some(T0);
    let mut discarded = run("a", RunState::Discarded);
    discarded.updated_ms = T0;
    let clean = run("a", RunState::Clean);
    for r in [&kept, &discarded, &clean] {
        s.insert_run(r, &launch).unwrap();
    }
    let report = s.prune(T0 + 91 * 86_400_000, i64::MAX).unwrap();
    assert_eq!(report.runs, 2);
    assert_eq!(s.list_runs(None, None, 0).unwrap().len(), 1);
}
