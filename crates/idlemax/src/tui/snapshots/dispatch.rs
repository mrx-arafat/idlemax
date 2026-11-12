//! Tests: the Dispatch tab's screens: every pane, the stop question, the limit input, a
//! read-only limit row and a refusal from the caller lock.

use ratatui::crossterm::event::KeyCode;
use serde_json::{json, Value};

use super::{app_with, check, fixture, parse, press, press_key, screen, HOUR, MIN, NOW};
use crate::tui::app::Tab;
use crate::tui::feed::{Snapshot, Update};

fn worker(run: i64, state: &str, label: &str, summary: &str, start: i64) -> Value {
    json!({"run": run, "device": "mac", "project": "app", "task": format!("roadmap:1:task-{run}"),
           "state": state, "label": label, "summary": summary,
           "why": "first roadmap item; the check is focused", "provider": "claude",
           "account": "max", "model": "sonnet", "branch": format!("idlemax/task-{run}"),
           "start_ms": start})
}

fn limit(key: &str, value: f64, from_file: bool) -> Value {
    json!({"key": key, "value": value, "default": 90.0, "from_file": from_file})
}

fn view() -> Value {
    let day = |ago: i64, spent: f64| {
        json!({"day_start_ms": NOW - ago * 24 * HOUR - HOUR,
        "spendable_pct": 6.0, "spent_pct": spent, "left_pct": 6.0 - spent,
        "why": "weekly 31% used, 52 h to reset: 6% a day keeps 15% for the owner"})
    };
    json!({"at_ms": NOW, "mode": "propose",
        "picked_by": "orchestrator pass 12 min ago on claude max (sonnet)",
        "devices": [{"id": "mac", "state": "run", "reason": "memory 41% used", "open_slots": 1,
                     "limits": [limit("dispatcher.hold_mem_used_pct", 85.0, true)]}],
        "projects": [
            {"id": 1, "name": "app", "device": "mac", "enabled": true, "waiting": 2, "review_cap": 3},
            {"id": 2, "name": "site", "device": "mac", "enabled": false, "waiting": 0, "review_cap": 3}],
        "planned": [{"device": "mac", "project": "app", "task": "roadmap:1:export-csv",
                     "title": "Export as CSV", "provider": "claude", "account": "max",
                     "model": "sonnet", "reason": "small and covered by tests"}],
        "ready_unjudged": [{"project": "site", "task": "roadmap:1:fix-footer", "title": "Fix the footer"}],
        "workers": [
            worker(1, "running", "working", "Add dark mode on claude sonnet", NOW - 31 * MIN),
            worker(2, "stopped", "stopped by you", "Speed up the build, saved at turn 4", NOW - HOUR)],
        "needs_you": [{"key": "task:app:release", "kind": "task", "subject": "app: Pick the release date",
                       "reason": "the item says it needs the owner"}],
        "review": [worker(3, "clean", "ready for review", "Fix the login test: checks passed", NOW - 2 * HOUR)],
        "budgets": [{"account": "max", "provider": "claude", "unit": "weekly_pct",
                     "days": [day(0, 2.5), day(1, 6.0), day(2, 4.1)]}],
        "proposals_open": 1})
}

fn dispatch() -> Snapshot {
    Snapshot {
        dispatcher: Some(parse(view())),
        limits: parse(json!([
            limit("dispatcher.hold_mem_used_pct", 88.0, true),
            limit("dispatcher.shed_mem_used_pct", 95.0, false),
            limit("workers.review_cap", 3.0, false)
        ])),
        proposals: parse(json!([{"id": 4, "at_ms": NOW - 5 * MIN, "run": 3,
            "line": "Never edit scripts/release.sh without asking.",
            "reason": "run 3 changed the release script and needed you", "status": "open"}])),
        ..fixture()
    }
}

#[test]
fn every_dispatch_pane_fits_80x24() {
    let mut out = String::new();
    let mut shot = |title: &str, app: &mut crate::tui::app::App| {
        out.push_str(&format!("##### {title}\n{}\n", screen(app, 80, 24)));
    };
    let mut app = app_with(dispatch(), Tab::Dispatch);
    for _ in 0..8 {
        let title = app.pane.title();
        shot(title, &mut app);
        press(&mut app, 'v');
    }
    for _ in 0..6 {
        press(&mut app, 'v');
    }
    for _ in 0..3 {
        press_key(&mut app, KeyCode::Down);
    }
    shot("limits: a read-only row", &mut app);
    press_key(&mut app, KeyCode::Up);
    press(&mut app, 'e');
    for c in "92".chars() {
        press(&mut app, c);
    }
    shot("limits: typing a value", &mut app);
    press_key(&mut app, KeyCode::Esc);
    press(&mut app, 'x');
    shot("the stop question", &mut app);
    press(&mut app, 'n');
    let refusal = "stop workers: /v1/workers/stop: HTTP 403: this call comes from a job or \
worker that idlemax started (pid 4242); only the owner can do this, from their own terminal, \
the dashboard or the TUI";
    app.apply(Update::Failed(refusal.into()), NOW);
    shot("a refusal from the caller lock", &mut app);
    check("dispatch-80x24", &out);
}
