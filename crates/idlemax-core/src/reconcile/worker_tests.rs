//! Tests: worker jobs in the agent tick: started by the line state, never held by the guard,
//! shed one at a time newest first until the device reads `run`, all stopped in an emergency,
//! and the owner's keep and discard never held (D14, A10, A16).

use super::*;
use crate::dispatch::room::{LineState, RoomDecision};
use crate::types::{GuardAction, GuardDecision, JobSpec};
use crate::worker::{WorkerAction, WorkerSpec};

const DEV: &str = "mac";

fn guard(action: GuardAction) -> GuardDecision {
    GuardDecision {
        action,
        resource_action: action,
        user_active: true,
        reasons: vec!["user active".into()],
        ..Default::default()
    }
}

fn room(state: LineState, at_ms: UnixMs) -> RoomDecision {
    RoomDecision {
        state,
        reason: format!("{} because memory", state.as_str()),
        open_slots: u32::from(state == LineState::Run),
        at_ms,
    }
}

fn worker(id: JobId, action: WorkerAction, priority: i32) -> Job {
    Job {
        id,
        spec: JobSpec {
            name: format!("w{id}"),
            priority,
            worker: Some(WorkerSpec {
                action,
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn plain(id: JobId) -> Job {
    Job {
        id,
        spec: JobSpec {
            name: format!("p{id}"),
            ..Default::default()
        },
        ..Default::default()
    }
}

struct In {
    guard: GuardDecision,
    room: RoomDecision,
    kill_switch: bool,
    mem: Vec<(JobId, u64)>,
}

impl In {
    fn new(action: GuardAction, line: LineState, at_ms: UnixMs) -> Self {
        Self {
            guard: guard(action),
            room: room(line, at_ms),
            kill_switch: false,
            mem: Vec::new(),
        }
    }

    fn input<'a>(&'a self, assign: &'a [Job]) -> TickInput<'a> {
        TickInput {
            guard: &self.guard,
            kill_switch: self.kill_switch,
            online: true,
            commands: &[],
            local_controls: &[],
            assign,
            shutting_down: false,
            lines: &self.guard,
            room: &self.room,
            worker_mem: &self.mem,
            others: "Safari 2.1 GiB",
        }
    }
}

fn tick(state: &mut AgentState, input: &TickInput<'_>, now_ms: UnixMs) -> Vec<AgentAction> {
    let actions = state.plan(input);
    for a in &actions {
        state.applied(a, Ok(()), &DEV.to_string(), now_ms);
    }
    actions
}

fn kills(actions: &[AgentAction]) -> Vec<JobId> {
    actions
        .iter()
        .filter_map(|a| match a {
            AgentAction::Kill { id, .. } => Some(*id),
            _ => None,
        })
        .collect()
}

const PAUSE: GuardAction = GuardAction::Pause;
const RUN: GuardAction = GuardAction::Run {
    slots: 9,
    mem_mib: 99_999,
};

#[test]
fn a_launch_follows_the_line_and_not_the_guard() {
    let mut state = AgentState::default();
    let at_run = In::new(PAUSE, LineState::Run, 1);
    let assign = [worker(1, WorkerAction::Launch, 0), plain(2)];
    let actions = tick(&mut state, &at_run.input(&assign), 1);
    assert!(matches!(&actions[0], AgentAction::Start(j) if j.id == 1));
    assert!(matches!(&actions[1], AgentAction::Return { id: 2, .. }));

    let mut held = AgentState::default();
    let at_hold = In::new(RUN, LineState::Hold, 1);
    let actions = held.plan(&at_hold.input(&[worker(3, WorkerAction::Launch, 0)]));
    assert!(
        matches!(&actions[0], AgentAction::Return { id: 3, reason } if reason.contains("hold"))
    );

    // The guard's pause never reaches it; the kill switch does.
    assert!(tick(&mut state, &at_run.input(&[]), 2).is_empty());
    let mut switched = In::new(PAUSE, LineState::Run, 3);
    switched.kill_switch = true;
    let actions = tick(&mut state, &switched.input(&[]), 3);
    assert_eq!(actions, [AgentAction::Pause(1)]);
}

#[test]
fn a_discard_starts_through_every_hold_and_survives_an_emergency() {
    let mut state = AgentState::default();
    let mut held = In::new(PAUSE, LineState::Hold, 1);
    held.kill_switch = true;
    let actions = tick(
        &mut state,
        &held.input(&[worker(1, WorkerAction::Discard, 0)]),
        1,
    );
    assert!(matches!(&actions[0], AgentAction::Start(_)));
    let emergency = In::new(PAUSE, LineState::Emergency, 2);
    assert!(tick(&mut state, &emergency.input(&[]), 2).is_empty());
}

fn three_workers(priorities: [i32; 3], starts: [UnixMs; 3]) -> AgentState {
    let mut state = AgentState::default();
    for (i, (p, at)) in priorities.into_iter().zip(starts).enumerate() {
        let id = i as JobId + 1;
        let run = In::new(RUN, LineState::Run, at);
        tick(
            &mut state,
            &run.input(&[worker(id, WorkerAction::Launch, p)]),
            at,
        );
    }
    state
}

#[test]
fn a_shed_stops_the_newest_then_the_lowest_priority_then_the_biggest() {
    let mut state = three_workers([0, 0, 0], [10, 20, 30]);
    assert_eq!(
        kills(&state.plan(&In::new(RUN, LineState::Shed, 40).input(&[]))),
        [3]
    );

    let mut state = three_workers([5, 1, 3], [10, 10, 10]);
    assert_eq!(
        kills(&state.plan(&In::new(RUN, LineState::Shed, 40).input(&[]))),
        [2]
    );

    let mut state = three_workers([0, 0, 0], [10, 10, 10]);
    let mut shed = In::new(RUN, LineState::Shed, 40);
    shed.mem = vec![(1, 100), (2, 900), (3, 300)];
    assert_eq!(kills(&state.plan(&shed.input(&[]))), [2]);
}

#[test]
fn a_shed_goes_on_one_at_a_time_until_the_device_reads_run() {
    let mut state = three_workers([0, 0, 0], [10, 20, 30]);
    let shed = In::new(RUN, LineState::Shed, 40);
    assert_eq!(kills(&tick(&mut state, &shed.input(&[]), 40)), [3]);
    // Worker 3 is still exiting, and no newer sample arrived.
    assert!(kills(&tick(&mut state, &shed.input(&[]), 41)).is_empty());
    let update = JobUpdate {
        id: 3,
        status: JobStatus::Killed,
        ..Default::default()
    };
    state.finished(update);
    assert!(kills(&tick(&mut state, &shed.input(&[]), 42)).is_empty());
    // A newer sample at hold, after a shed, still sheds one more.
    let hold = In::new(RUN, LineState::Hold, 50);
    assert_eq!(kills(&tick(&mut state, &hold.input(&[]), 50)), [2]);
    state.finished(JobUpdate {
        id: 2,
        status: JobStatus::Killed,
        ..Default::default()
    });
    let run = In::new(RUN, LineState::Run, 60);
    assert!(kills(&tick(&mut state, &run.input(&[]), 60)).is_empty());
    let hold = In::new(RUN, LineState::Hold, 70);
    assert!(kills(&tick(&mut state, &hold.input(&[]), 70)).is_empty());
}

#[test]
fn an_emergency_stops_every_worker_at_once_and_no_plain_job() {
    let mut state = three_workers([0, 0, 0], [10, 20, 30]);
    let run = In::new(RUN, LineState::Run, 35);
    tick(&mut state, &run.input(&[plain(9)]), 35);
    let actions = tick(
        &mut state,
        &In::new(RUN, LineState::Emergency, 40).input(&[]),
        40,
    );
    assert_eq!(kills(&actions), [1, 2, 3]);
    for action in &actions {
        if let AgentAction::Kill { order, .. } = action {
            assert!(order.retryable);
            assert!(order.reason.contains("Safari 2.1 GiB"), "{}", order.reason);
        }
    }
}

#[test]
fn a_hold_without_a_shed_before_it_stops_nothing() {
    let mut state = three_workers([0, 0, 0], [10, 20, 30]);
    assert!(kills(&state.plan(&In::new(RUN, LineState::Hold, 40).input(&[]))).is_empty());
}

#[test]
fn workers_taken_back_after_a_restart_keep_their_real_start_for_the_shed_order() {
    let mut state = AgentState::default();
    // The older worker has the higher id, so only the real start times can pick the newer.
    let mut older = worker(2, WorkerAction::Launch, 0);
    older.started_ms = Some(100);
    let mut newer = worker(1, WorkerAction::Launch, 0);
    newer.started_ms = Some(200);
    // Taken back in the newer-first order, both at the same restart time.
    state.adopt(&newer, false, 5_000);
    state.adopt(&older, false, 5_000);
    let shed = In::new(RUN, LineState::Shed, 6_000);
    assert_eq!(kills(&state.plan(&shed.input(&[]))), [1]);
}
