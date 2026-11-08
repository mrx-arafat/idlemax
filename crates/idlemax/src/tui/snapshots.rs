//! Tests: whole-screen renders of the dashboard from one fixed dataset, compared with
//! the text files in `snapshots/`. After a deliberate layout change, review the new
//! screens and accept them with `UPDATE_SNAPSHOTS=1 cargo test -p idlemax --bin idlemax tui::`.

use idlemax_core::types::UnixMs;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, Tab};
use super::feed::{Snapshot, Update};
use super::view;

const NOW: UnixMs = 1_800_000_000_000;
const MIN: i64 = 60_000;
const HOUR: i64 = 60 * MIN;

fn parse<T: DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).expect("fixture matches the API shape")
}

fn healthy_mac() -> Value {
    json!({
        "info": {"id": "macbook-pro-arafat", "hostname": "macbook-pro-arafat.local", "os": "macos",
                 "cpu_count": 10, "agent_version": "0.1.0", "profile": "macos_laptop"},
        "online": true, "last_seen_ms": NOW - 2_000,
        "metrics": {"cpu_count": 10, "cpu_busy_pct": 23.4, "load_avg_1m": 2.31, "mem_free_pct": 41.0,
                    "mem_available_bytes": 6_700_000_000u64, "swap_used_bytes": 0,
                    "disk_free_bytes": 210_000_000_000u64, "user_idle_secs": 3720,
                    "on_ac_power": true, "battery_pct": 87.0},
        "guard": {"action": {"kind": "run", "slots": 2, "mem_mib": 4096},
                  "reasons": ["run: all signals healthy"]},
        "running": [{"id": 41, "name": "index-refresh", "status": "running"}]
    })
}

fn loaded_server() -> Value {
    json!({
        "info": {"id": "hetzner-fsn1-build-server-eu-central-01", "os": "linux", "cpu_count": 8},
        "online": true, "last_seen_ms": NOW - 5_000, "local_paused": true,
        "metrics": {"cpu_count": 8, "cpu_busy_pct": 96.0, "load_avg_1m": 9.4, "mem_free_pct": 8.0,
                    "mem_available_bytes": 1_300_000_000u64, "swap_used_bytes": 2_400_000_000u64,
                    "disk_free_bytes": 35_000_000_000u64, "on_ac_power": null},
        "guard": {"action": {"kind": "pause"},
                  "reasons": ["pause: load 1m 9.40 is above 8.00 on 8 cores for 3 minutes in a row"]},
        "running": [{"id": 43, "name": "kb-refresh", "status": "paused"}]
    })
}

fn offline_mini() -> Value {
    json!({"info": {"id": "old-mac-mini", "os": "macos"}, "online": false,
           "last_seen_ms": NOW - 49 * HOUR})
}

fn job(id: i64, name: &str, status: &str, extra: Value) -> Value {
    let mut j = json!({"id": id, "spec": {"name": name, "command": ["true"]}, "status": status,
                       "created_ms": NOW - id * MIN});
    j.as_object_mut()
        .expect("object")
        .extend(extra.as_object().cloned().unwrap_or_default());
    j
}

fn jobs() -> Value {
    let long = "pr-review-drafts-claude-for-a-really-long-repository-name-12345";
    json!([
        job(46, "index-refresh", "queued", json!({})),
        job(
            45,
            long,
            "parked",
            json!({"attempts": 1, "last_error": "ai_account claude-work: quota reserve not met, 3% headroom left in the 5 hour window"})
        ),
        job(
            44,
            "repo-health-project-a",
            "failed",
            json!({"device": "hetzner-fsn1-build-server-eu-central-01", "attempts": 3, "exit_code": 2, "finished_ms": NOW - 3 * MIN})
        ),
        job(
            43,
            "kb-refresh",
            "paused",
            json!({"device": "hetzner-fsn1-build-server-eu-central-01", "started_ms": NOW - 30 * MIN})
        ),
        job(
            41,
            "index-refresh",
            "running",
            json!({"device": "macbook-pro-arafat", "started_ms": NOW - 12 * MIN, "attempts": 1})
        ),
        job(
            40,
            "nightly-audit",
            "succeeded",
            json!({"device": "macbook-pro-arafat", "exit_code": 0, "finished_ms": NOW - 5 * HOUR})
        ),
    ])
}

fn quota() -> Value {
    json!([
        {"account": "claude-work", "provider": "claude", "window": {"kind": "rolling", "minutes": 300},
         "used_pct": 62.0, "reserve_pct": 20.0, "headroom_pct": 18.0, "resets_at_ms": NOW + 95 * MIN,
         "spendable": false, "reasons": ["held: 18% above the reserve is below the 25% minimum"]},
        {"account": "claude-work", "provider": "claude", "window": {"kind": "weekly"},
         "used_pct": 31.0, "reserve_pct": 15.0, "headroom_pct": 54.0, "resets_at_ms": NOW + 52 * HOUR,
         "spendable": true, "reasons": []},
        {"account": "codex-personal-with-a-very-long-account-name", "provider": "codex",
         "window": {"kind": "weekly"}, "reserve_pct": 15.0, "spendable": false,
         "reasons": ["no usage data yet"]},
    ])
}

fn advice() -> Value {
    json!([
        {"id": 7, "at_ms": NOW - 20 * MIN, "kind": "fix", "status": "open",
         "title": "Job 45 is parked: the claude-work account has no headroom left this window",
         "why": "The job needs the 5 hour window but only 3% is left above the reserve; it resets in 1h35m.",
         "evidence": ["window 5h used 97%", "reserve 20%", "resets in 1h35m"],
         "confidence": 0.82, "source": "rules",
         "suggested_job": {"name": "pr-review-drafts-claude", "command": ["claude", "-p", "review the open PRs"]}},
        {"id": 6, "at_ms": NOW - 3 * HOUR, "kind": "do", "status": "dismissed", "title": "Refresh the KB index",
         "why": "Not refreshed for 2 days.", "confidence": 0.5, "source": "ai"},
    ])
}

fn events() -> Value {
    json!([
        {"id": 5, "at_ms": NOW - MIN, "kind": "error", "device": "hetzner-fsn1-build-server-eu-central-01", "job": 44,
         "message": "job 44 failed: repo-health-project-a exited with code 2 after 3 attempts, see the inbox note for the last 40 lines of output"},
        {"id": 4, "at_ms": NOW - 3 * MIN, "kind": "schedule_fired", "job": 46, "message": "schedule 1 (index-refresh) queued job 46"},
        {"id": 3, "at_ms": NOW - 12 * MIN, "kind": "job_started", "device": "macbook-pro-arafat", "job": 41, "message": "started"},
        {"id": 2, "at_ms": NOW - 49 * HOUR, "kind": "device_offline", "device": "old-mac-mini", "message": "no heartbeat for 90s"},
    ])
}

fn schedules() -> Value {
    let view = |id: i64, name: &str, every: &str, paused: bool, decision: Value, last: Value| {
        json!({"id": id, "spec": {"name": name, "command": ["true"]}, "every": every, "paused": paused,
               "created_ms": 0, "last_fired_ms": NOW - HOUR, "last_job": 40 + id, "decision": decision,
               "last_result": last})
    };
    json!([
        view(
            1,
            "index-refresh",
            "12h",
            false,
            json!({"kind": "running", "job": 46, "status": "queued"}),
            Value::Null
        ),
        view(
            2,
            "repo-health-project-a-with-a-long-schedule-name-here",
            "1d",
            true,
            json!({"kind": "paused"}),
            json!({"status": "failed", "exit_code": 2, "error": null, "finished_ms": NOW - 3 * MIN})
        ),
        view(
            3,
            "kb-refresh",
            "6h",
            false,
            json!({"kind": "wait", "until_ms": NOW + 5 * HOUR + 41 * MIN}),
            json!({"status": "succeeded", "exit_code": 0, "error": null, "finished_ms": NOW - 19 * MIN})
        ),
    ])
}

fn fixture() -> Snapshot {
    let status = json!({
        "version": "0.1.0", "paused_accounts": ["claude-work"], "open_advice": 1,
        "jobs": {"queued": 1, "running": 1, "paused": 1, "failed": 1, "parked": 1},
        "devices": [healthy_mac(), loaded_server(), offline_mini()],
    });
    Snapshot {
        status: parse(status),
        jobs: parse(jobs()),
        quota: parse(quota()),
        advice: parse(advice()),
        events: parse(events()),
        schedules: parse(schedules()),
    }
}

fn app_with(data: Snapshot, tab: Tab) -> App {
    let mut app = App::new("http://127.0.0.1:7878");
    app.apply(Update::Snapshot(Box::new(data), NOW - 2_000), NOW);
    app.tab = tab;
    app
}

/// The screen as text: one line per row, trailing blanks trimmed.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(|f| view::render(f, app, NOW)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            let row: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const UNREACHABLE: &str = "cannot reach http://127.0.0.1:7878: error sending request for url \
(http://127.0.0.1:7878/v1/status): client error (Connect): tcp connect error: \
Connection refused (os error 61)";

fn press(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), NOW);
}

/// Every tab on its own screen, one after the other.
fn all_tabs(data: fn() -> Snapshot, width: u16, height: u16) -> String {
    Tab::ALL
        .iter()
        .map(|tab| {
            let mut app = app_with(data(), *tab);
            format!(
                "##### {} {width}x{height}\n{}\n",
                tab.title(),
                screen(&mut app, width, height)
            )
        })
        .collect()
}

/// The screens that are not a table: connecting, errors, confirmations and flashes.
fn states(width: u16, height: u16) -> String {
    let mut out = String::new();
    let mut shot = |title: &str, app: &mut App| {
        out.push_str(&format!("##### {title}\n{}\n", screen(app, width, height)));
    };
    shot("connecting", &mut App::new("http://127.0.0.1:7878"));

    let mut app = App::new("http://127.0.0.1:7878");
    app.apply(Update::FetchFailed(UNREACHABLE.into()), NOW);
    shot("unreachable, nothing loaded yet", &mut app);

    let mut app = app_with(fixture(), Tab::Jobs);
    app.apply(Update::FetchFailed(UNREACHABLE.into()), NOW);
    shot("unreachable, showing the last data", &mut app);

    let mut app = app_with(fixture(), Tab::Jobs);
    press(&mut app, 's');
    shot("confirm stop all", &mut app);

    let mut app = app_with(fixture(), Tab::Schedules);
    app.tables[Tab::Schedules.index()].select(Some(1));
    press(&mut app, 'd');
    shot("confirm remove a schedule with a long name", &mut app);

    let mut app = app_with(fixture(), Tab::Jobs);
    app.apply(Update::Done("pause schedule 3".into()), NOW);
    shot("flash after an action", &mut app);

    let mut app = app_with(fixture(), Tab::Jobs);
    let failure = "remove schedule 3: /v1/schedules/3: HTTP 404: no schedule with id 3; it may \
have been removed from another terminal, so press tab to reload the list and try again";
    app.apply(Update::Failed(failure.into()), NOW);
    shot("flash after a failed action", &mut app);
    out
}

fn empty() -> Snapshot {
    Snapshot {
        status: parse(json!({"version": "0.1.0"})),
        ..Snapshot::default()
    }
}

fn check(name: &str, actual: &str) {
    let path = format!(
        "{}/src/tui/snapshots/{name}.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        actual == expected,
        "{name} changed; review the screens below, then accept with UPDATE_SNAPSHOTS=1\n{actual}"
    );
}

#[test]
fn every_tab_fits_80x24() {
    check("tabs-80x24", &all_tabs(fixture, 80, 24));
}

#[test]
fn empty_tabs_say_what_to_do_at_80x24() {
    check("empty-80x24", &all_tabs(empty, 80, 24));
}

#[test]
fn errors_confirmations_and_flashes_fit_80x24() {
    check("states-80x24", &states(80, 24));
}

#[test]
fn wide_terminal_keeps_every_column_and_count() {
    let mut out = String::new();
    for tab in [Tab::Devices, Tab::Jobs] {
        let mut app = app_with(fixture(), tab);
        out.push_str(&format!(
            "##### {} 120x40\n{}\n",
            tab.title(),
            screen(&mut app, 120, 40)
        ));
    }
    check("wide-120x40", &out);
}
