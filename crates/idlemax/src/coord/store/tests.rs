//! Tests: store lease compare-and-set against real SQLite.

use idlemax_core::types::{CostClass, Job, JobSpec, JobStatus};

use super::jobs::{Added, LIVE};
use super::{schema, Caps, Store, StoreError, MAX_EVENT_MESSAGE_CHARS};

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
    assert!(s.apply_control(&plan, T0).is_err());
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

#[test]
fn a_key_holds_one_unfinished_job_and_frees_when_it_ends() {
    let mut s = Store::open_in_memory().unwrap();
    let keyed = |name: &str, key: Option<&str>| JobSpec {
        key: key.map(Into::into),
        ..spec(name, None)
    };
    let Added::New(first) = s.insert_job_once(&keyed("a", Some("pr-1")), T0).unwrap() else {
        panic!("first add must queue");
    };

    // Even a changed spec with the same key resolves to the first job.
    let again = s
        .insert_job_once(&keyed("b", Some("pr-1")), T0 + 1)
        .unwrap();
    assert_eq!(again, Added::Existing(first.clone()));
    let running = Job {
        status: JobStatus::Running,
        ..first.clone()
    };
    s.update_jobs(std::slice::from_ref(&running)).unwrap();
    let again = s
        .insert_job_once(&keyed("b", Some("pr-1")), T0 + 2)
        .unwrap();
    assert_eq!(again, Added::Existing(running.clone()));

    // Other keys and keyless specs never collide.
    for key in [Some("pr-2"), None, None] {
        assert!(matches!(
            s.insert_job_once(&keyed("c", key), T0 + 3).unwrap(),
            Added::New(_)
        ));
    }

    let done = Job {
        status: JobStatus::Succeeded,
        ..running
    };
    s.update_jobs(&[done]).unwrap();
    let Added::New(next) = s
        .insert_job_once(&keyed("a", Some("pr-1")), T0 + 4)
        .unwrap()
    else {
        panic!("a finished job frees its key");
    };
    assert_ne!(next.id, first.id);
    assert_eq!(s.list_jobs(None, 0).unwrap().len(), 5);
}

#[test]
fn the_unfinished_statuses_match_the_domain() {
    for status in JobStatus::ALL {
        let listed = LIVE.contains(&format!("'{status}'"));
        assert_eq!(listed, !status.is_terminal(), "{status}");
    }
}

#[test]
fn a_v5_database_upgrades_and_keeps_its_jobs() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    for sql in &schema::MIGRATIONS[..5] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", 5).unwrap();
    conn.execute(
        "INSERT INTO jobs (spec, name, status, priority, created_ms)
         VALUES ('{\"name\":\"old\",\"command\":[\"true\"]}', 'old', 'queued', 0, ?1)",
        [T0],
    )
    .unwrap();

    let mut s = Store::init(conn, false).unwrap();
    let old = s.get_job(1).unwrap().unwrap();
    assert_eq!(
        (old.spec.name.as_str(), old.spec.key.as_deref()),
        ("old", None)
    );
    let keyed = JobSpec {
        key: Some("k".into()),
        ..spec("new", None)
    };
    assert!(matches!(
        s.insert_job_once(&keyed, T0).unwrap(),
        Added::New(_)
    ));
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

#[test]
fn lease_resets_attempt_fields_on_retry() {
    let mut s = Store::open_in_memory().unwrap();
    let job = s.insert_job(&spec("retry-me", None), T0).unwrap();
    let retried = Job {
        status: JobStatus::Queued,
        attempts: 1,
        started_ms: Some(T0 + 1000),
        finished_ms: Some(T0 + 2000),
        exit_code: Some(1),
        output_path: Some("/tmp/out".into()),
        ..job
    };
    s.update_jobs(&[retried]).unwrap();

    let leased = s
        .lease_job(job.id, "mac", T0 + 120_000, T0 + 3000)
        .unwrap()
        .unwrap();
    assert_eq!(leased.attempts, 2);
    assert_eq!(leased.started_ms, None);
    assert_eq!(leased.finished_ms, None);
    assert_eq!(leased.exit_code, None);
    assert_eq!(leased.output_path, None);
}

fn capped(caps: Caps) -> Store {
    Store::open_in_memory().unwrap().with_caps(caps)
}

fn beat(id: &str) -> idlemax_core::protocol::Heartbeat {
    idlemax_core::protocol::Heartbeat {
        device: idlemax_core::types::DeviceInfo {
            id: id.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn limit_hit(result: Result<impl std::fmt::Debug, StoreError>, what: &str) {
    match result {
        Err(StoreError::LimitReached { what: got, .. }) => assert_eq!(got, what),
        other => panic!("wanted the {what} limit, got {other:?}"),
    }
}

#[test]
fn events_are_clipped_and_only_the_newest_are_kept() {
    use idlemax_core::types::EventKind;
    let mut s = capped(Caps {
        events: 3,
        ..Caps::default()
    });
    for i in 0..5 {
        s.append_event(
            EventKind::Control,
            None,
            None,
            &format!("event {i}"),
            T0 + i,
        )
        .unwrap();
    }
    let long = s
        .append_event(EventKind::Control, None, None, &"x".repeat(2000), T0 + 9)
        .unwrap();
    assert_eq!(long.message.chars().count(), MAX_EVENT_MESSAGE_CHARS);
    let kept = s.events(0).unwrap();
    assert_eq!(kept.len(), 3, "only the newest 3 remain");
    assert_eq!(kept[0].message.chars().count(), MAX_EVENT_MESSAGE_CHARS);
    assert_eq!(kept[1].message, "event 4");
}

#[test]
fn a_new_device_over_the_cap_is_refused_and_a_known_one_still_reports() {
    let mut s = capped(Caps {
        devices: 2,
        ..Caps::default()
    });
    s.record_heartbeat(&beat("a"), T0).unwrap();
    s.record_heartbeat(&beat("b"), T0).unwrap();
    limit_hit(s.record_heartbeat(&beat("c"), T0), "devices");
    s.record_heartbeat(&beat("a"), T0 + 1).unwrap();
}

#[test]
fn unfinished_jobs_over_the_cap_are_refused_and_finished_ones_do_not_count() {
    let mut s = capped(Caps {
        live_jobs: 2,
        ..Caps::default()
    });
    let first = s.insert_job(&spec("one", None), T0).unwrap();
    s.insert_job(&spec("two", None), T0).unwrap();
    limit_hit(s.insert_job(&spec("three", None), T0), "queued jobs");
    limit_hit(s.insert_job_once(&spec("four", None), T0), "queued jobs");
    s.update_jobs(&[Job {
        status: JobStatus::Cancelled,
        finished_ms: Some(T0),
        ..first
    }])
    .unwrap();
    s.insert_job(&spec("three", None), T0).unwrap();
}

#[test]
fn pause_flags_over_the_cap_are_refused_and_resuming_still_works() {
    use idlemax_core::control::ControlPlan;
    use idlemax_core::types::ControlScope;
    let mut s = capped(Caps {
        controls: 2,
        ..Caps::default()
    });
    let plan = |scope: &str, paused| ControlPlan {
        pause: Some((ControlScope::Device(scope.into()), paused)),
        jobs: Vec::new(),
        summary: String::new(),
    };
    s.apply_control(&plan("a", true), T0).unwrap();
    s.apply_control(&plan("b", true), T0).unwrap();
    limit_hit(s.apply_control(&plan("c", true), T0), "pause flags");
    s.apply_control(&plan("a", true), T0).unwrap();
    s.apply_control(&plan("a", false), T0).unwrap();
    s.apply_control(&plan("c", true), T0).unwrap();
}

#[test]
fn job_pause_flags_cleared_on_terminal_and_prune() {
    use idlemax_core::control::ControlPlan;
    use idlemax_core::types::ControlScope;
    let mut s = Store::open_in_memory().unwrap();
    let job1 = s.insert_job(&spec("job1", None), T0).unwrap();
    let job2 = s.insert_job(&spec("job2", None), T0).unwrap();

    let pause = |id: i64, paused| ControlPlan {
        pause: Some((ControlScope::Job(id), paused)),
        jobs: Vec::new(),
        summary: String::new(),
    };
    s.apply_control(&pause(job1.id, true), T0).unwrap();
    s.apply_control(&pause(job2.id, true), T0).unwrap();
    assert_eq!(s.paused_scopes().unwrap().len(), 2);

    let mut finished = job1;
    finished.status = JobStatus::Succeeded;
    finished.finished_ms = Some(T0 + 1000);
    s.update_jobs(&[finished]).unwrap();

    let paused = s.paused_scopes().unwrap();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0], ControlScope::Job(job2.id));
}

#[test]
fn prune_forgets_silent_devices_and_old_finished_jobs_but_keeps_parked_and_scheduled_ones() {
    const DAY: i64 = 86_400_000;
    let mut s = Store::open_in_memory().unwrap();
    let now = T0 + 100 * DAY;
    s.record_heartbeat(&beat("gone"), now - 31 * DAY).unwrap();
    s.record_heartbeat(&beat("here"), now - DAY).unwrap();

    let finish = |s: &mut Store, name: &str, status: JobStatus, ended: i64| {
        let job = s.insert_job(&spec(name, None), T0).unwrap();
        let ended = Job {
            status,
            finished_ms: Some(ended),
            ..job
        };
        s.update_jobs(std::slice::from_ref(&ended)).unwrap();
        ended.id
    };
    let old = finish(&mut s, "old", JobStatus::Succeeded, now - 91 * DAY);
    let recent = finish(&mut s, "recent", JobStatus::Succeeded, now - 10 * DAY);
    let parked = finish(&mut s, "parked", JobStatus::Parked, now - 200 * DAY);
    let sched: idlemax_core::schedule::Every = "1d".parse().unwrap();
    let schedule = s
        .insert_schedule(&spec("nightly", None), sched, T0)
        .unwrap();
    let fired = s.fire_schedule(&schedule, T0).unwrap();
    s.update_jobs(&[Job {
        status: JobStatus::Succeeded,
        finished_ms: Some(now - 200 * DAY),
        ..fired.clone()
    }])
    .unwrap();

    let report = s.prune(now, 7 * DAY).unwrap();
    assert_eq!((report.devices, report.jobs), (1, 1), "{report:?}");
    let exists = |s: &Store, id| s.get_job(id).unwrap().is_some();
    assert!(!exists(&s, old), "a finished job past 90 days goes");
    assert!(
        exists(&s, recent) && exists(&s, parked),
        "recent and parked stay"
    );
    assert!(exists(&s, fired.id), "a schedule's last job stays");
    let names: Vec<_> = s
        .devices(now, 1)
        .unwrap()
        .into_iter()
        .map(|d| d.info.id)
        .collect();
    assert_eq!(names, ["here"]);
}

#[test]
fn heartbeat_room_round_trips() {
    use idlemax_core::dispatch::room::{LineState, RoomDecision};
    use idlemax_core::protocol::Heartbeat;
    let mut s = Store::open_in_memory().unwrap();
    let mut hb = Heartbeat::default();
    hb.device.id = "mac".into();
    hb.room = Some(RoomDecision {
        state: LineState::Run,
        reason: "run: memory 80% used".into(),
        open_slots: 3,
        at_ms: T0,
    });
    s.record_heartbeat(&hb, T0).unwrap();
    let devices = s.devices(T0, 90_000).unwrap();
    assert_eq!(devices[0].room, hb.room);
    let old: Heartbeat = serde_json::from_str(r#"{"device":{"id":"mac"}}"#).unwrap();
    assert_eq!(old.room, None);
}
