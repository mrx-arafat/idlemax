//! Tests: the Run's state moves and how each job update moves it.

use super::*;

#[test]
fn may_become_allows_exactly_the_listed_moves() {
    use RunState::*;
    let allowed = [
        (Starting, Running),
        (Starting, Stopped),
        (Starting, Shed),
        (Starting, NeedsYou),
        (Running, Checking),
        (Running, Stopped),
        (Running, Shed),
        (Running, NeedsYou),
        (Checking, Clean),
        (Checking, NeedsYou),
        (Checking, Rejected),
        (Checking, Stopped),
        (Checking, Shed),
        (Stopped, Shed),
        (Stopped, Discarded),
        (Shed, Starting),
        (Shed, Discarded),
        (Clean, Kept),
        (Clean, Discarded),
        (NeedsYou, Kept),
        (NeedsYou, Discarded),
        (Rejected, Kept),
        (Rejected, Discarded),
    ];
    for from in RunState::ALL {
        for to in RunState::ALL {
            assert_eq!(
                from.may_become(to),
                allowed.contains(&(from, to)),
                "{from} -> {to}"
            );
        }
    }
}

#[test]
fn states_round_trip_as_snake_case() {
    for st in RunState::ALL {
        let json = serde_json::to_value(st).unwrap();
        assert_eq!(json, st.as_str());
        assert_eq!(st.as_str().parse::<RunState>().unwrap(), st);
    }
}

mod after_job {
    use super::super::*;
    use crate::protocol::JobUpdate;
    use crate::types::{CostClass, Job, JobSpec, JobStatus};
    use crate::worker::report::{WorkerPhase, WorkerReport};
    use crate::worker::WorkerAction;

    fn run(state: RunState) -> Run {
        Run {
            id: 4,
            state,
            job: Some(9),
            launches: 1,
            ..Default::default()
        }
    }

    fn job(action: WorkerAction) -> Job {
        Job {
            id: 9,
            spec: JobSpec {
                cost: CostClass::Ai,
                worker: Some(WorkerSpec {
                    run: 4,
                    action,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn update(status: JobStatus) -> JobUpdate {
        JobUpdate {
            id: 9,
            status,
            at_ms: 50,
            error: Some("boom".into()),
            ..Default::default()
        }
    }

    fn with_phase(mut u: JobUpdate, phase: WorkerPhase) -> JobUpdate {
        u.worker = Some(Box::new(WorkerReport {
            run: 4,
            phase,
            session: Some("sess".into()),
            branch: Some("idlemax/t".into()),
            worktree: Some("/w".into()),
            ..Default::default()
        }));
        u
    }

    fn moved(r: Run, u: JobUpdate) -> Option<Run> {
        after_job(&r, &job(WorkerAction::Launch), &u, 70)
    }

    #[test]
    fn the_running_phase_gives_running_and_stores_what_it_reports() {
        let mut starting = run(RunState::Starting);
        starting.start_tries = 2;
        let u = with_phase(update(JobStatus::Running), WorkerPhase::Running);
        let next = moved(starting, u).unwrap();
        assert_eq!(next.state, RunState::Running);
        assert_eq!(next.start_tries, 0);
        assert_eq!(next.running_ms, Some(70), "when the worker first ran");
        assert_eq!(next.session.as_deref(), Some("sess"));
        assert_eq!(next.branch.as_deref(), Some("idlemax/t"));
        assert_eq!(next.worktree.as_deref(), Some("/w"));
        assert_eq!(next.updated_ms, 70);
    }

    #[test]
    fn the_preparing_phase_stores_the_session_and_keeps_the_state() {
        let u = with_phase(update(JobStatus::Running), WorkerPhase::Preparing);
        let next = moved(run(RunState::Starting), u).unwrap();
        assert_eq!(next.state, RunState::Starting);
        assert_eq!(next.session.as_deref(), Some("sess"));
    }

    #[test]
    fn the_checking_phase_gives_checking() {
        let u = with_phase(update(JobStatus::Running), WorkerPhase::Checking);
        let next = moved(run(RunState::Running), u).unwrap();
        assert_eq!(next.state, RunState::Checking);
    }

    #[test]
    fn a_returned_lease_a_retryable_end_and_an_expired_lease_give_shed() {
        let mut retryable = update(JobStatus::Killed);
        retryable.retryable = true;
        for u in [update(JobStatus::Queued), retryable] {
            let next = moved(run(RunState::Running), u).unwrap();
            assert_eq!(next.state, RunState::Shed);
            assert_eq!(next.start_tries, 0);
        }
        let next = after_expiry(&run(RunState::Running), &job(WorkerAction::Launch), 70).unwrap();
        assert_eq!(next.state, RunState::Shed);
    }

    #[test]
    fn the_third_start_that_fails_in_a_row_needs_the_owner() {
        let mut r = run(RunState::Starting);
        for tries in 1..START_TRIES {
            r = moved(r, update(JobStatus::Queued)).unwrap();
            assert_eq!((r.state, r.start_tries), (RunState::Shed, tries));
            r.state = RunState::Starting;
        }
        let r = moved(r, update(JobStatus::Queued)).unwrap();
        assert_eq!(r.state, RunState::NeedsYou);
        assert_eq!(
            r.concern.as_deref(),
            Some("could not be started 3 times: boom")
        );
        assert_eq!(r.end_ms, Some(70));
    }

    #[test]
    fn an_owner_stop_gives_stopped() {
        let mut u = update(JobStatus::Killed);
        u.owner_stop = true;
        u.retryable = true;
        let next = moved(run(RunState::Running), u).unwrap();
        assert_eq!(next.state, RunState::Stopped);
    }

    #[test]
    fn a_failed_job_or_a_final_kill_needs_the_owner_with_its_error() {
        for status in [JobStatus::Failed, JobStatus::Killed] {
            let next = moved(run(RunState::Running), update(status)).unwrap();
            assert_eq!(next.state, RunState::NeedsYou, "{status}");
            assert_eq!(next.concern.as_deref(), Some("boom"));
        }
    }

    #[test]
    fn a_succeeded_launch_gets_the_verdict_of_its_facts() {
        use crate::worker::report::{CheckFacts, DiffStat, FileDiff};

        let facts = CheckFacts {
            commits: 1,
            diff: DiffStat {
                files: vec![FileDiff {
                    path: "src/a.rs".into(),
                    added: 1,
                    removed: 0,
                }],
            },
            summary: Some("add a line".into()),
            baseline_before: Some(0),
            baseline_after: Some(0),
            ..Default::default()
        };
        let mut u = with_phase(update(JobStatus::Succeeded), WorkerPhase::Done);
        if let Some(w) = u.worker.as_mut() {
            w.facts = Some(facts.clone());
            w.end_text = Some("I could not find the cause".into());
        }
        let mut j = job(WorkerAction::Launch);
        if let Some(w) = j.spec.worker.as_mut() {
            w.scope_paths = vec!["src/a.rs".into()];
            w.check = vec!["make".into()];
            w.review_diff_lines = 300;
        }
        let next = after_job(&run(RunState::Running), &j, &u, 70).unwrap();
        assert_eq!(next.state, RunState::Clean);
        assert_eq!(next.verdict, Some(Verdict::Clean));
        assert_eq!(next.summary.as_deref(), Some("add a line"));
        assert_eq!(next.facts.check, Some(facts));
        assert_eq!(
            next.facts.end_text.as_deref(),
            Some("I could not find the cause")
        );
        assert_eq!(next.end_ms, Some(70));

        // A turn a usage limit cut goes back in the queue and is never a failed check.
        if let Some(w) = u.worker.as_mut() {
            w.end_text = Some("usage limit reached".into());
        }
        let next = after_job(&run(RunState::Running), &j, &u, 70).unwrap();
        assert_eq!((next.state, next.verdict), (RunState::Shed, None));

        let bare = with_phase(update(JobStatus::Succeeded), WorkerPhase::Done);
        let next = after_job(&run(RunState::Checking), &j, &bare, 70).unwrap();
        assert_eq!(next.state, RunState::NeedsYou);
        assert!(next.concern.unwrap().contains("no check facts"));
    }

    #[test]
    fn a_stopped_run_does_not_move_and_other_jobs_leave_it_alone() {
        let u = with_phase(update(JobStatus::Running), WorkerPhase::Running);
        assert_eq!(moved(run(RunState::Stopped), u.clone()), None);
        let keep = job(WorkerAction::Keep);
        assert_eq!(after_job(&run(RunState::Clean), &keep, &u, 70), None);
        let mut older = run(RunState::Running);
        older.job = Some(3);
        assert_eq!(moved(older, u), None);
    }

    #[test]
    fn a_succeeded_keep_or_discard_ends_the_run_and_a_failed_one_leaves_it() {
        let mut clean = run(RunState::Clean);
        clean.verdict = Some(Verdict::Clean);
        for (action, want) in [
            (WorkerAction::Keep, RunState::Kept),
            (WorkerAction::KeepMerge, RunState::Kept),
            (WorkerAction::Discard, RunState::Discarded),
        ] {
            let next = after_job(&clean, &job(action), &update(JobStatus::Succeeded), 70).unwrap();
            assert_eq!((next.state, next.verdict), (want, Some(Verdict::Clean)));
            assert_eq!(
                after_job(&clean, &job(action), &update(JobStatus::Failed), 70),
                None
            );
        }
        let shed = run(RunState::Shed);
        let gone = after_job(
            &shed,
            &job(WorkerAction::Discard),
            &update(JobStatus::Succeeded),
            70,
        );
        assert_eq!(gone.unwrap().state, RunState::Discarded);
    }
}

#[test]
fn a_long_end_text_is_clipped() {
    use crate::protocol::JobUpdate;
    use crate::types::{CostClass, Job, JobSpec, JobStatus};
    use crate::worker::report::{WorkerReport, MAX_END_TEXT_CHARS};

    let run = Run {
        state: RunState::Running,
        job: Some(9),
        ..Default::default()
    };
    let job = Job {
        id: 9,
        spec: JobSpec {
            cost: CostClass::Ai,
            worker: Some(WorkerSpec::default()),
            ..Default::default()
        },
        ..Default::default()
    };
    let update = JobUpdate {
        id: 9,
        status: JobStatus::Succeeded,
        worker: Some(Box::new(WorkerReport {
            end_text: Some("é".repeat(MAX_END_TEXT_CHARS + 10)),
            ..Default::default()
        })),
        ..Default::default()
    };
    let next = after_job(&run, &job, &update, 1).unwrap();
    let kept = next.facts.end_text.unwrap();
    assert_eq!(kept.chars().count(), MAX_END_TEXT_CHARS);
}

#[test]
fn the_baseline_commit_is_bounded() {
    use crate::types::{CostClass, JobSpec, SpecError};
    use crate::worker::Baseline;

    let spec = JobSpec {
        name: "w".into(),
        command: vec!["idlemax-worker".into()],
        cost: CostClass::Ai,
        ai_account: Some("max".into()),
        worker: Some(WorkerSpec {
            baseline: Some(Baseline {
                base_commit: "a".repeat(5000),
                passed: None,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(matches!(spec.validate(), Err(SpecError::OverLimit { .. })));
}
