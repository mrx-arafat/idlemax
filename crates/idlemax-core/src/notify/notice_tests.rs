use super::*;
use crate::dispatch::view::NeedKind;
use crate::worker::run::RunState;
use crate::worker::WorkerSpec;

const MIN: UnixMs = 60_000;
const T0: UnixMs = 1_800_000_000_000;

fn run(id: i64, state: RunState, verdict: Option<Verdict>, at: UnixMs) -> Run {
    Run {
        id,
        project: "app".into(),
        task_title: format!("task {id}"),
        state,
        verdict,
        end_ms: Some(at),
        updated_ms: at,
        ..Run::default()
    }
}

fn job(name: &str, status: JobStatus, at: UnixMs) -> Job {
    let mut job = Job {
        id: 9,
        status,
        finished_ms: Some(at),
        ..Job::default()
    };
    job.spec.name = name.into();
    job
}

#[test]
fn only_finished_tasks_and_plain_jobs_are_items() {
    let clean = Some(Verdict::Clean);
    for r in [
        run(1, RunState::Starting, None, T0),
        run(2, RunState::Running, None, T0),
        run(3, RunState::Shed, None, T0),
        run(4, RunState::Stopped, None, T0),
        run(5, RunState::Rejected, Some(Verdict::Rejected), T0),
    ] {
        assert_eq!(run_item(&r), None, "{:?}", r.state);
    }
    let item = run_item(&run(6, RunState::Clean, clean, T0)).unwrap();
    assert_eq!(item.key, "run:6");
    assert_eq!(item.text, "task 6: checks passed - ready for review");

    let mut worker = job("worker:app:1", JobStatus::Killed, T0);
    worker.spec.worker = Some(WorkerSpec::default());
    assert!(!is_plain_job(&worker.spec));
    assert_eq!(job_item(&worker), None);
    assert!(!is_plain_job(
        &job("dispatch-check:app", JobStatus::Succeeded, T0).spec
    ));
    let advisor = job(
        crate::advice::ai::AI_PASS_JOB_NAME,
        JobStatus::Succeeded,
        T0,
    );
    assert!(!is_plain_job(&advisor.spec));
    assert_eq!(job_item(&job("backup", JobStatus::Running, T0)), None);
    assert_eq!(
        job_item(&job("backup", JobStatus::Succeeded, T0))
            .unwrap()
            .key,
        "job:9"
    );
}

#[test]
fn finished_tasks_wait_at_most_ten_minutes_and_share_one_message() {
    let clean = Some(Verdict::Clean);
    let items: Vec<Item> = [0, 4, 9, 12]
        .iter()
        .map(|m| run_item(&run(*m, RunState::Clean, clean, T0 + m * MIN)).unwrap())
        .collect();
    assert_eq!(finished_batch(&items, T0 + 9 * MIN), None);
    let first = finished_batch(&items, T0 + 10 * MIN).unwrap();
    assert_eq!(first.keys, ["run:0", "run:4", "run:9"]);
    assert_eq!(first.title, "idlemax: 3 tasks finished");
    assert_eq!(first.body.lines().count(), 3);
    let rest = &items[3..];
    assert_eq!(finished_batch(rest, T0 + 21 * MIN), None);
    let second = finished_batch(rest, T0 + 22 * MIN).unwrap();
    assert_eq!(
        (second.title.as_str(), second.keys.len()),
        ("idlemax: app", 1)
    );

    let both = [
        run_item(&run(1, RunState::NeedsYou, Some(Verdict::NeedsYou), T0)).unwrap(),
        job_item(&job("backup", JobStatus::Failed, T0 + 2 * MIN)).unwrap(),
    ];
    let draft = finished_batch(&both, T0 + 12 * MIN).unwrap();
    assert_eq!(draft.title, "idlemax: 2 tasks finished");
    assert_eq!(
        draft.body,
        "app: task 1: no reason given - needs you\nbackup: failed"
    );
}

#[test]
fn many_needs_you_lines_are_one_message_of_ten_lines() {
    let lines: Vec<NeedsYouLine> = (0..30)
        .map(|i| NeedsYouLine {
            key: format!("task:1:t{i}"),
            kind: NeedKind::Task,
            subject: format!("app: t{i}"),
            reason: "vague".into(),
        })
        .collect();
    assert_eq!(needs_batch(&[]), None);
    let draft = needs_batch(&lines).unwrap();
    assert_eq!(draft.title, "idlemax needs you: 30 things");
    let body: Vec<&str> = draft.body.lines().collect();
    assert_eq!(body.len(), 11);
    assert_eq!(body[10], "and 20 more: idlemax status");
    assert_eq!(draft.keys.len(), 30);
    let one = needs_batch(&lines[..1]).unwrap();
    assert_eq!(
        (one.title.as_str(), one.body.as_str()),
        ("idlemax needs you: app: t0", "vague")
    );
}

#[test]
fn the_daily_summary_comes_after_nine_and_only_for_a_day_with_something() {
    // 2027-01-15 09:00 at UTC+2.
    let offset = 2 * 3600;
    let nine = chrono::DateTime::parse_from_rfc3339("2027-01-15T09:00:00+02:00")
        .unwrap()
        .timestamp_millis();
    let facts = DayFacts {
        tasks_done: 2,
        accounts: vec![("max".into(), 6.0, Some(8.0)), ("pro".into(), 1.0, None)],
        waiting: 1,
    };
    assert_eq!(daily_summary(&facts, nine - MIN, offset), None);
    let draft = daily_summary(&facts, nine, offset).unwrap();
    assert_eq!(draft.keys, ["daily:2027-01-15"]);
    assert_eq!(
        draft.body,
        "Tasks done: 2\nmax: spent 6% of 8% budget\npro: spent 1%\nWaiting for review: 1"
    );
    assert_eq!(
        summary_window(nine + MIN, offset),
        Some((nine - 86_400_000, nine))
    );
    let empty = DayFacts {
        accounts: vec![("max".into(), 0.0, None)],
        ..DayFacts::default()
    };
    assert_eq!(daily_summary(&empty, nine, offset), None);
}
