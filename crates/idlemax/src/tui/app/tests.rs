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

/// A coordinator with the dispatcher view: one project off, two limits and an open proposal.
fn dispatch_app() -> App {
    let view = json!({"at_ms": 0, "mode": "off", "picked_by": "no orchestrator pass",
        "devices": [{"id": "mac", "state": "run", "reason": "room for 1", "open_slots": 1}],
        "projects": [{"id": 2, "name": "site", "device": "mac", "enabled": false}]});
    let limit = |key: &str| json!({"key": key, "value": 90.0, "default": 90.0, "from_file": false});
    let snap = Snapshot {
        dispatcher: Some(serde_json::from_value(view).unwrap()),
        limits: serde_json::from_value(json!([limit("workers.review_cap"), limit("x.y")])).unwrap(),
        proposals: serde_json::from_value(json!([{"id": 4, "at_ms": 0, "run": 3,
            "line": "never touch release.sh", "reason": "run 3 changed it", "status": "open"}]))
        .unwrap(),
        advice: serde_json::from_value(json!([{"id": 7, "at_ms": 0, "kind": "fix",
            "status": "open", "title": "t", "why": "w", "confidence": 0.5, "source": "rules"}]))
        .unwrap(),
        ..Snapshot::default()
    };
    let mut app = App::new("test");
    app.apply(Update::Snapshot(Box::new(snap), 0), 0);
    press(&mut app, '7');
    app
}

/// Moves the Dispatch tab to the pane with this title.
fn pane(app: &mut App, title: &str) {
    for _ in 0..8 {
        if app.pane.title() == title {
            return;
        }
        press(app, 'v');
    }
    panic!("no pane {title}");
}

fn key(app: &mut App, code: KeyCode) -> Option<Request> {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE), 0)
}

#[test]
fn x_asks_once_before_it_stops_the_workers_on_every_tab() {
    let mut app = dispatch_app();
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let digit = char::from_digit(i as u32 + 1, 10).unwrap();
        press(&mut app, digit);
        assert_eq!(app.tab, *tab);
        assert_eq!(press(&mut app, 'x'), None, "{tab:?}");
        let prompt = app.confirm.as_ref().map(|c| c.prompt.as_str());
        assert_eq!(
            prompt,
            Some("Stop every worker now? They continue only after start."),
            "{tab:?}"
        );
        assert_eq!(press(&mut app, 'y'), Some(Request::StopWorkers), "{tab:?}");
    }
    press(&mut app, 'x');
    assert_eq!(press(&mut app, 'n'), None);
}

#[test]
fn a_limit_is_typed_in_the_footer_and_sent_once() {
    let mut app = dispatch_app();
    pane(&mut app, "limits");
    assert_eq!(press(&mut app, 'e'), None);
    for c in "q88".chars() {
        assert_eq!(press(&mut app, c), None);
    }
    key(&mut app, KeyCode::Backspace);
    press(&mut app, '7');
    let sent = key(&mut app, KeyCode::Enter);
    assert_eq!(
        sent,
        Some(Request::SetLimit {
            key: "workers.review_cap".into(),
            value: Some("q87".into())
        })
    );
    assert!(app.input.is_none() && !app.quit);

    press(&mut app, 'e');
    press(&mut app, '1');
    assert_eq!(key(&mut app, KeyCode::Esc), None);
    assert!(app.input.is_none() && !app.quit);

    // A slip of Enter sends nothing; a reset to the default needs the word.
    press(&mut app, 'e');
    assert_eq!(key(&mut app, KeyCode::Enter), None);
    assert!(app.input.is_none());
    press(&mut app, 'e');
    for c in "default".chars() {
        press(&mut app, c);
    }
    let reset = Request::SetLimit {
        key: "workers.review_cap".into(),
        value: None,
    };
    assert_eq!(key(&mut app, KeyCode::Enter), Some(reset));
}

#[test]
fn a_and_d_decide_the_rule_proposal_before_the_advice_keys() {
    let mut app = dispatch_app();
    pane(&mut app, "rules");
    assert_eq!(press(&mut app, 'a'), None);
    assert_eq!(press(&mut app, 'y'), Some(Request::ApproveRule(4)));
    assert_eq!(press(&mut app, 'd'), Some(Request::RejectRule(4)));
}

#[test]
fn switching_on_and_toward_auto_ask_first_and_switching_off_does_not() {
    let mut app = dispatch_app();
    pane(&mut app, "projects");
    assert_eq!(press(&mut app, 't'), None);
    let on = Request::SwitchProject {
        id: 2,
        name: "site".into(),
        on: true,
    };
    assert_eq!(press(&mut app, 'y'), Some(on));
    assert_eq!(press(&mut app, 'm'), None);
    assert_eq!(
        press(&mut app, 'y'),
        Some(Request::SetMode("propose".into()))
    );
    assert_eq!(press(&mut app, 'g'), None);
    assert_eq!(press(&mut app, 'y'), Some(Request::StartWorkers));
}

#[test]
fn without_a_dispatcher_view_no_key_on_the_tab_sends_a_request() {
    let mut app = App::new("test");
    app.apply(Update::Snapshot(Box::default(), 0), 0);
    press(&mut app, '7');
    assert_eq!(app.tab, Tab::Dispatch);
    for c in "mgtevadpPRsk".chars() {
        assert_eq!(press(&mut app, c), None, "{c}");
        assert!(app.confirm.is_none(), "{c} asked");
    }
    // The stop key is on every tab: it is a different route from the view.
    press(&mut app, 'x');
    assert!(app.confirm.is_some());
}
