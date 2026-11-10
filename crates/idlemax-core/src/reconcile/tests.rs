//! Unit tests for agent tick decisions.

use super::*;
use crate::types::JobSpec;

const DEV: &str = "mac";

#[test]
fn starts_after_clock_rollback_still_consume_the_sample_budget() {
    let mut state = AgentState::default();
    let guard = run(2, 200, 1_000);
    tick(
        &mut state,
        &TickInput {
            assign: &[job(1, 100, ActivePolicy::Pause)],
            ..input(&guard)
        },
        500,
    );
    let actions = state.plan(&TickInput {
        assign: &[
            job(2, 100, ActivePolicy::Pause),
            job(3, 100, ActivePolicy::Pause),
        ],
        ..input(&guard)
    });
    assert!(matches!(actions[0], AgentAction::Start(_)));
    assert!(matches!(actions[1], AgentAction::Return { id: 3, .. }));
}

fn run(slots: u32, mem_mib: u64, at_ms: UnixMs) -> GuardDecision {
    let action = GuardAction::Run { slots, mem_mib };
    GuardDecision {
        action,
        resource_action: action,
        at_ms,
        ..GuardDecision::default()
    }
}

fn verdict(action: GuardAction, user_active: bool, resource_action: GuardAction) -> GuardDecision {
    GuardDecision {
        memory_limit: None,
        action,
        resource_action,
        user_active,
        reasons: vec!["load 9 > 8".into()],
        at_ms: 100,
    }
}

fn job(id: JobId, mem_mb: u64, on_user_active: ActivePolicy) -> Job {
    Job {
        id,
        spec: JobSpec {
            mem_mb,
            on_user_active,
            ..JobSpec::default()
        },
        ..Job::default()
    }
}

fn input<'a>(guard: &'a GuardDecision) -> TickInput<'a> {
    TickInput {
        guard,
        kill_switch: false,
        online: true,
        commands: &[],
        local_controls: &[],
        assign: &[],
        shutting_down: false,
    }
}

/// Plans one tick and applies every action as successful at `now_ms`.
fn tick(state: &mut AgentState, input: &TickInput<'_>, now_ms: UnixMs) -> Vec<AgentAction> {
    let actions = state.plan(input);
    for a in &actions {
        state.applied(a, Ok(()), &DEV.to_string(), now_ms);
    }
    actions
}

fn started(jobs: &[Job]) -> AgentState {
    let mut state = AgentState::default();
    let g = run(9, 9999, 0);
    tick(
        &mut state,
        &TickInput {
            assign: jobs,
            ..input(&g)
        },
        1,
    );
    state
}

#[test]
fn tick_pauses_resumes_kills_starts_and_returns_leases() {
    use ActivePolicy::{Continue, Kill, Pause};
    // Guard pause pauses; a later run resumes.
    let mut state = started(&[job(1, 100, Pause)]);
    let pause = verdict(GuardAction::Pause, false, GuardAction::Pause);
    assert_eq!(
        tick(&mut state, &input(&pause), 2),
        vec![AgentAction::Pause(1)]
    );
    assert_eq!(
        tick(&mut state, &input(&run(1, 512, 4)), 4),
        vec![AgentAction::Resume(1)]
    );

    // User active: each job follows its on_user_active policy.
    let mut state = started(&[job(1, 100, Pause), job(2, 100, Continue), job(3, 100, Kill)]);
    let user_active = verdict(GuardAction::Pause, true, GuardAction::Hold);
    let guard_kill = KillOrder::retryable("guard: load 9 > 8");
    assert_eq!(
        tick(&mut state, &input(&user_active), 2),
        vec![
            AgentAction::Pause(1),
            AgentAction::Kill {
                id: 3,
                order: guard_kill
            }
        ]
    );

    // Starts fit the slots and RAM left after jobs started since the guard sample.
    let mut state = AgentState::default();
    let g = run(2, 600, 50);
    tick(
        &mut state,
        &TickInput {
            assign: &[job(1, 300, Pause)],
            ..input(&g)
        },
        60,
    );
    let next = [job(1, 300, Pause), job(2, 400, Pause), job(3, 200, Pause)];
    assert_eq!(
        state.plan(&TickInput {
            assign: &next,
            ..input(&g)
        }),
        vec![
            AgentAction::Return {
                id: 2,
                reason: "needs 400 MiB, guard allows 300 MiB".into()
            },
            AgentAction::Start(Box::new(job(3, 200, Pause))),
        ]
    );

    // Offline, kill switch or guard hold: new leases go back unstarted.
    let a = [job(7, 100, Pause)];
    let g = run(1, 512, 0);
    let hold = verdict(GuardAction::Hold, false, GuardAction::Hold);
    let cases = [
        (
            TickInput {
                online: false,
                assign: &a,
                ..input(&g)
            },
            "coordinator unreachable",
        ),
        (
            TickInput {
                kill_switch: true,
                assign: &a,
                ..input(&g)
            },
            "kill switch file present",
        ),
        (
            TickInput {
                assign: &a,
                ..input(&hold)
            },
            "guard: load 9 > 8",
        ),
    ];
    for (inp, reason) in cases {
        let want = vec![AgentAction::Return {
            id: 7,
            reason: reason.into(),
        }];
        assert_eq!(AgentState::default().plan(&inp), want, "{reason}");
    }

    // Local stop-all kills as an owner stop and holds new leases.
    let mut state = started(&[job(1, 100, Continue)]);
    let stop = [ControlAction::StopAll];
    let actions = tick(
        &mut state,
        &TickInput {
            local_controls: &stop,
            assign: &[job(2, 100, Pause)],
            ..input(&g)
        },
        11,
    );
    assert_eq!(
        actions,
        vec![
            AgentAction::Kill {
                id: 1,
                order: KillOrder::new("stop_all from local control", true)
            },
            AgentAction::Return {
                id: 2,
                reason: "paused locally".into()
            },
        ]
    );
    let killed = JobUpdate {
        id: 1,
        status: JobStatus::Killed,
        ..JobUpdate::default()
    };
    let owner_stopped = state.finished(killed);
    assert!(owner_stopped.owner_stop && !owner_stopped.retryable);
}

/// Of everything that ends a job, only what is not the job's fault is marked `retryable`.
#[test]
fn only_an_attempt_the_agent_cut_short_is_marked_retryable() {
    use ActivePolicy::{Kill, Pause};
    let killed = |error: Option<&str>| JobUpdate {
        id: 1,
        status: JobStatus::Killed,
        error: error.map(Into::into),
        ..JobUpdate::default()
    };
    let user_active = verdict(GuardAction::Pause, true, GuardAction::Hold);

    // The guard kills a job whose owner came back: not the job's fault.
    let mut state = started(&[job(1, 100, Kill)]);
    tick(&mut state, &input(&user_active), 2);
    let update = state.finished(killed(None));
    assert!(update.retryable && !update.owner_stop, "{update:?}");
    assert_eq!(update.error.as_deref(), Some("guard: load 9 > 8"));

    // The agent shutting down is not either.
    let mut state = started(&[job(1, 100, Pause)]);
    let g = run(1, 512, 4);
    tick(
        &mut state,
        &TickInput {
            shutting_down: true,
            ..input(&g)
        },
        3,
    );
    assert!(state.finished(killed(None)).retryable);

    // A job that broke its own memory cap or runtime limit is final, even when a guard
    // kill happened to be pending; so is a kill the coordinator ordered.
    let mut state = started(&[job(1, 100, Kill)]);
    tick(&mut state, &input(&user_active), 2);
    assert!(
        !state
            .finished(killed(Some("memory 900 MiB over the 100 MiB cap")))
            .retryable
    );
    let mut state = started(&[job(1, 100, Pause)]);
    let command = [AgentCommand::KillJob { id: 1 }];
    tick(
        &mut state,
        &TickInput {
            commands: &command,
            ..input(&g)
        },
        3,
    );
    assert!(!state.finished(killed(None)).retryable);
}

#[test]
fn updates_carry_the_attempt_they_belong_to() {
    let mut leased = job(1, 100, ActivePolicy::Pause);
    leased.attempts = 2;
    let g = run(9, 9999, 0);
    let mut state = AgentState::default();
    let plan = state.plan(&TickInput {
        assign: std::slice::from_ref(&leased),
        ..input(&g)
    });
    let start = &plan[0];
    let running = state.applied(start, Ok(()), &DEV.to_string(), 1).unwrap();
    assert_eq!(running.attempt, 2);

    let refused = AgentState::default()
        .applied(start, Err("no".into()), &DEV.to_string(), 1)
        .unwrap();
    assert_eq!((refused.status, refused.attempt), (JobStatus::Failed, 2));
    // The agent could not start it, which says nothing about the job.
    assert!(refused.retryable);
}
