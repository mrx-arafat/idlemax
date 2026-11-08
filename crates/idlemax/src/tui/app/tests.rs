use idlemax_core::explain::Explanation;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

use super::{App, Request, Snapshot, Tab, Update};

fn press(app: &mut App, c: char) -> Option<Request> {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), 0)
}

fn app_with_schedule() -> App {
    let view = json!({
        "id": 3, "spec": {"name": "repo-health", "command": ["true"]}, "every": "1d",
        "created_ms": 0, "last_fired_ms": null, "last_job": null,
        "decision": {"kind": "fire"}
    });
    let mut app = App::new("test");
    let snap = Snapshot {
        schedules: vec![serde_json::from_value(view).unwrap()],
        ..Snapshot::default()
    };
    app.apply(Update::Snapshot(Box::new(snap), 0), 0);
    app
}

#[test]
fn schedules_tab_pauses_resumes_and_confirms_removal_of_the_selected_row() {
    let mut app = app_with_schedule();
    press(&mut app, '6');
    assert_eq!(app.tab, Tab::Schedules);
    assert_eq!(press(&mut app, 'p'), Some(Request::PauseSchedule(3)));
    assert_eq!(press(&mut app, 'r'), Some(Request::ResumeSchedule(3)));

    assert_eq!(press(&mut app, 'd'), None);
    let prompt = app.confirm.as_ref().map(|c| c.prompt.as_str());
    assert_eq!(
        prompt,
        Some("Remove schedule 3 (repo-health)? A job it already queued keeps running.")
    );
    assert_eq!(press(&mut app, 'y'), Some(Request::RemoveSchedule(3)));
}

fn jobs_with_headline(message: &str) -> Snapshot {
    let job = |id: i64, status: &str| json!({"id": id, "spec": {"name": format!("job-{id}"), "command": ["true"]}, "status": status});
    let summary = json!({"job": 1, "status": "queued", "state": "blocked", "more": 0, "headline": {
            "device": null,
            "reason": {"kind": "guard", "message": message, "fix": null, "until_ms": null}}});
    Snapshot {
        jobs: serde_json::from_value(json!([job(1, "queued"), job(2, "running")])).unwrap(),
        why: serde_json::from_value(json!([summary])).unwrap(),
        ..Snapshot::default()
    }
}

fn explanation(job: i64) -> Explanation {
    serde_json::from_value(json!({"job": job, "name": "job", "status": "queued", "state": "blocked",
            "summary": "Job 1 is queued but not starting.", "reasons": [], "devices": [], "at_ms": 0}))
        .unwrap()
}

#[test]
fn why_pane_asks_for_the_selected_waiting_job_only_when_its_reasons_changed() {
    let mut app = App::new("test");
    app.apply(
        Update::Snapshot(Box::new(jobs_with_headline("on battery")), 0),
        0,
    );
    app.tab = Tab::Jobs;
    assert_eq!(press(&mut app, 'w'), Some(Request::Why(1)));
    app.apply(Update::Why(1, Ok(explanation(1))), 0);
    assert_eq!(app.why.as_ref().map(|w| w.job), Some(1));

    // A running job has nothing to explain; coming back to job 1 is free.
    let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
    let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.on_key(down, 0), None);
    assert_eq!(app.on_key(up, 0), None);

    // The same reasons on the next refresh cost nothing; changed reasons are fetched again.
    let same = Update::Snapshot(Box::new(jobs_with_headline("on battery")), 0);
    assert_eq!(app.apply(same, 0), None);
    let changed = Update::Snapshot(Box::new(jobs_with_headline("load is high")), 0);
    assert_eq!(app.apply(changed, 0), Some(Request::Why(1)));

    // Closing and reopening asks again.
    assert_eq!(press(&mut app, 'w'), None);
    assert_eq!(press(&mut app, 'w'), Some(Request::Why(1)));
}
