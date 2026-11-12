use super::*;
use crate::dispatch::plan::DevicePlan;
use crate::dispatch::project::Project;
use crate::dispatch::room::RoomDecision;
use crate::dispatch::task::{Task, TaskSource};

const DAY: UnixMs = 86_400_000;
const NOW: UnixMs = 100 * DAY;

fn run(id: RunId, state: RunState, at: UnixMs) -> RunLine {
    RunLine {
        run: Run {
            id,
            device: "mac".into(),
            project: "app".into(),
            task: format!("t{id}"),
            task_title: format!("task {id}"),
            state,
            reason: format!("why {id}"),
            start_ms: at,
            updated_ms: at,
            ..Run::default()
        },
        frozen: false,
    }
}

fn task(id: &str, needs_you: Option<&str>) -> Task {
    Task {
        id: id.into(),
        project: "app".into(),
        project_id: 1,
        device: "mac".into(),
        source: TaskSource::Roadmap,
        title: format!("title {id}"),
        done_check: None,
        scope_paths: Vec::new(),
        needs_you: needs_you.map(str::to_string),
    }
}

fn plan(state: LineState, needs_you: Vec<Task>) -> Plan {
    Plan {
        at_ms: NOW,
        mode: Mode::Propose,
        picked_by: "no pass yet".into(),
        devices: vec![DevicePlan {
            device: "mac".into(),
            room: RoomDecision {
                state,
                reason: format!("memory {state:?}"),
                open_slots: 0,
                at_ms: NOW,
            },
            held: None,
            picks: Vec::new(),
            empty: None,
            enabled: None,
        }],
        not_judged: Vec::new(),
        needs_you,
        accounts: Vec::new(),
        notes: Vec::new(),
    }
}

fn budget(account: &str, basis: BudgetBasis, rests_until_ms: Option<UnixMs>) -> Budget {
    Budget {
        account: account.into(),
        provider: Provider::Claude,
        unit: BudgetUnit::WeeklyPct,
        day_start_ms: NOW - DAY / 2,
        day_end_ms: NOW + DAY / 2,
        spendable_pct: 8.0,
        spent_pct: 2.0,
        left_pct: 6.0,
        usable: false,
        rests_until_ms,
        owner_active: false,
        cap_runs: None,
        runs_today: 0,
        costs: Vec::new(),
        basis,
        why: format!("{account} {basis:?}"),
    }
}

fn projects() -> [ProjectView; 1] {
    [ProjectView {
        project: Project {
            id: 1,
            name: "app".into(),
            device: "mac".into(),
            enabled: true,
            ..Project::default()
        },
        ..ProjectView::default()
    }]
}

fn view(plan: &Plan, runs: &[RunLine], budgets: &[Budget], days: &[AccountDay]) -> DispatcherView {
    let projects = projects();
    let input = ViewInput {
        plan,
        runs,
        projects: &projects,
        budgets,
        days,
        review_cap: 3,
        proposals_open: 0,
    };
    dispatcher_view(&input, NOW)
}

#[test]
fn workers_are_the_live_stopped_and_shed_runs_newest_first_and_waiting_counts_two_states() {
    let runs: Vec<RunLine> = RunState::ALL
        .iter()
        .enumerate()
        .map(|(i, s)| run(i as RunId + 1, *s, i as UnixMs))
        .chain([run(11, RunState::Clean, 20)])
        .chain([{
            // Same project name on another device: not this project's.
            let mut other = run(12, RunState::Clean, 21);
            other.run.device = "box".into();
            other
        }])
        .collect();
    let v = view(&plan(LineState::Run, Vec::new()), &runs, &[], &[]);

    let states: Vec<RunState> = v.workers.iter().map(|w| w.state).collect();
    use RunState::*;
    assert_eq!(states, [Shed, Stopped, Checking, Running, Starting]);
    let shed = &v.workers[0];
    assert_eq!(shed.label, run_label(Shed, false));
    assert_eq!(shed.summary, run_summary(&runs[7]));
    assert_eq!(shed.why, "why 8");

    let review: Vec<RunState> = v.review.iter().map(|w| w.state).collect();
    assert_eq!(review, [Clean, Clean, Rejected, NeedsYou, Clean]);
    // Two clean and one needs_you count; rejected does not.
    assert_eq!(v.projects[0].waiting, 3);
    assert_eq!(v.projects[0].review_cap, 3);
}

#[test]
fn needs_you_holds_tasks_resting_accounts_and_emergencies_and_nothing_else() {
    let budgets = [
        budget("past", BudgetBasis::Resting, Some(NOW + DAY)),
        budget("today", BudgetBasis::Resting, Some(NOW + DAY / 4)),
        budget("spent", BudgetBasis::Spent, None),
    ];
    let runs = [run(1, RunState::Starting, 1), run(2, RunState::Shed, 2)];
    let tasks = vec![task("t9\u{1b}[2J", Some("needs a\ndecision"))];
    let v = view(&plan(LineState::Emergency, tasks), &runs, &budgets, &[]);
    let keys: Vec<&str> = v.needs_you.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(
        keys,
        ["task:1:t9\u{1b}[2J", "account:past", "device:mac:emergency"]
    );
    // Text from roadmap files and issues reaches a terminal: no escape, no line break.
    assert_eq!(v.needs_you[0].subject, "app: title t9 [2J");
    assert_eq!(v.needs_you[0].reason, "needs a decision");

    // Spec 5 sets `enabled`; a device switched off with a project on is the fourth kind.
    let off = DeviceLine {
        id: "mac".into(),
        enabled: Some(false),
        ..DeviceLine::default()
    };
    let lines = device_needs(&[off], &projects());
    assert_eq!(lines[0].key, "device:mac:off");
    assert_eq!(
        lines[0].reason,
        "off, with 1 projects switched on: idlemax device on mac"
    );

    let calm = view(
        &plan(LineState::Hold, Vec::new()),
        &runs,
        &budgets[1..],
        &[],
    );
    assert!(calm.needs_you.is_empty(), "{:?}", calm.needs_you);
}

#[test]
fn a_budget_shows_today_then_the_earlier_days_newest_first() {
    let day = |start, pct| AccountDay {
        account: "acc".into(),
        day_start_ms: start,
        idlemax_pct: Some(pct),
        closed: true,
        ..AccountDay::default()
    };
    let today = budget("acc", BudgetBasis::Learned, None);
    let start = today.day_start_ms;
    let days = [day(start - 2 * DAY, 3.0), day(start - DAY, 5.0)];
    let v = view(&plan(LineState::Run, Vec::new()), &[], &[today], &days);

    let line = &v.budgets[0];
    let starts: Vec<UnixMs> = line.days.iter().map(|d| d.day_start_ms).collect();
    assert_eq!(starts, [start, start - DAY, start - 2 * DAY]);
    assert_eq!(line.days[0].spendable_pct, Some(8.0));
    assert_eq!(line.days[1].spent_pct, Some(5.0));
    assert_eq!(line.days[1].spendable_pct, None);
}

#[test]
fn a_plan_over_an_off_device_with_a_project_on_needs_you() {
    let mut off = plan(LineState::Run, Vec::new());
    off.devices[0].enabled = Some(false);
    let v = view(&off, &[], &[], &[]);
    assert_eq!(v.devices[0].enabled, Some(false));
    let keys: Vec<&str> = v.needs_you.iter().map(|n| n.key.as_str()).collect();
    assert!(keys.contains(&"device:mac:off"), "{keys:?}");
}
