//! Tests: whole-screen renders of the dashboard from one fixed dataset, compared with
//! the text files in `snapshots/`. After a deliberate layout change, review the new
//! screens and accept them with `UPDATE_SNAPSHOTS=1 cargo test -p idlemax --bin idlemax tui::`.

use idlemax_core::types::UnixMs;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use unicode_width::UnicodeWidthStr;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, Tab};
use crate::cli::rows::Zone;
use idlemax_core::explain::Explanation;

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
        "running": [{"id": 41, "name": "index-refresh", "status": "running"}],
        "capacity": {"state": "starting", "slots_used": 1, "slots_allowed": 3, "mem_used_mib": 512,
                     "mem_allowed_mib": 4608, "why": "starting 1 queued job on the next heartbeat"}
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
        "running": [{"id": 43, "name": "kb-refresh", "status": "paused"}],
        "capacity": {"state": "held", "slots_used": 1, "slots_allowed": null, "mem_used_mib": 512,
                     "mem_allowed_mib": null, "why": "held: a local pause"}
    })
}

fn offline_mini() -> Value {
    json!({"info": {"id": "old-mac-mini", "os": "macos"}, "online": false,
           "last_seen_ms": NOW - 49 * HOUR,
           "capacity": {"state": "offline", "slots_used": 0, "mem_used_mib": 0, "why": "offline"}})
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

fn reason(kind: &str, message: &str, fix: Option<&str>) -> Value {
    json!({"kind": kind, "message": message, "fix": fix, "until_ms": null})
}

/// What GET /v1/why answers for the fixture's waiting jobs: 46 is queued, 43 is paused.
fn why_summaries() -> Value {
    json!([
        {"job": 46, "status": "queued", "state": "blocked", "more": 1, "headline": {
            "device": "macbook-pro-arafat",
            "reason": reason("guard", "battery is at 28%, below the 40% minimum (guard pause)",
                             Some("plug in, or lower guard.battery_min_pct"))}},
        {"job": 43, "status": "paused", "state": "active", "more": 0, "headline": {
            "device": "hetzner-fsn1-build-server-eu-central-01",
            "reason": reason("guard", "load 1m 9.40 is above 8.00 (guard pause)", None)}},
    ])
}

/// GET /v1/jobs/46/why: three devices, none can take the job.
fn explanation_of_job_46() -> Explanation {
    parse(json!({
        "job": 46, "name": "index-refresh", "status": "queued", "state": "blocked", "at_ms": NOW,
        "summary": "Job 46 (index-refresh) is queued but not starting.",
        "reasons": [],
        "devices": [
            {"device": "macbook-pro-arafat", "online": true, "last_seen_ms": NOW - 2_000, "ready": false,
             "reasons": [
                reason("guard", "battery is at 28%, below the 40% minimum (guard pause)",
                       Some("plug in, or lower guard.battery_min_pct in config.toml")),
                reason("no_slot", "all 1 job slots are taken, by job 41 (index-refresh)", None)]},
            {"device": "hetzner-fsn1-build-server-eu-central-01", "online": true, "last_seen_ms": NOW - 5_000,
             "ready": false, "reasons": [
                reason("local_pause", "paused on the device itself (kill switch)",
                       Some("run `idlemax local resume` on that device")),
                reason("guard", "load 1m 9.40 is above 8.00 (guard pause)",
                       Some("wait for the load to drop, or raise guard.load_1m_pause"))]},
            {"device": "old-mac-mini", "online": false, "last_seen_ms": NOW - 49 * HOUR, "ready": false,
             "reasons": [reason("offline", "no heartbeat for 2d1h",
                                Some("start `idlemax agent` on old-mac-mini"))]}
        ]
    }))
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
        why: parse(why_summaries()),
    }
}

fn app_with(data: Snapshot, tab: Tab) -> App {
    let mut app = App::new("http://127.0.0.1:7878");
    // UTC, so a snapshot reads the same wherever the test runs (CI is in UTC).
    app.zone = Zone::Utc;
    app.apply(Update::Snapshot(Box::new(data), NOW - 2_000), NOW);
    app.tab = tab;
    app
}

/// The screen as text: one line per row, trailing blanks trimmed. A wide character
/// fills two cells, so the blank cell behind it is skipped and the text lines up.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(|f| view::render(f, app, NOW)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            let mut row = String::new();
            let mut covered = 0;
            for x in 0..width {
                if covered > 0 {
                    covered -= 1;
                    continue;
                }
                let symbol = buf[(x, y)].symbol();
                covered = symbol.width().saturating_sub(1);
                row.push_str(symbol);
            }
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

fn press_key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE), NOW);
}

/// The Jobs tab with the reasons pane open on job 46 (the first row), before any answer.
fn why_pane() -> App {
    let mut app = app_with(fixture(), Tab::Jobs);
    press(&mut app, 'w');
    app
}

/// The reasons screens: the answer, scrolled, still loading, failed, and a job with nothing to explain.
fn why_screens(width: u16, height: u16, all: bool) -> String {
    let mut out = String::new();
    let mut shot = |title: &str, app: &mut App| {
        out.push_str(&format!("##### {title}\n{}\n", screen(app, width, height)));
    };
    let mut app = why_pane();
    app.apply(Update::Why(46, Ok(explanation_of_job_46())), NOW);
    shot("reasons for a queued job", &mut app);
    if !all {
        return out;
    }
    press_key(&mut app, KeyCode::PageDown);
    shot("scrolled with PgDn", &mut app);
    press_key(&mut app, KeyCode::PageDown);
    press_key(&mut app, KeyCode::PageDown);
    shot("scrolled to the end", &mut app);
    shot("loading", &mut why_pane());
    let mut app = why_pane();
    app.apply(
        Update::Why(46, Err("/v1/jobs/46/why: HTTP 500: store is busy".into())),
        NOW,
    );
    shot("could not load", &mut app);
    let mut app = why_pane();
    for _ in 0..4 {
        press_key(&mut app, KeyCode::Down);
    }
    shot("a running job is selected", &mut app);
    out
}

/// Every tab on its own screen, one after the other.
fn all_tabs(data: fn() -> Snapshot, width: u16, height: u16) -> String {
    tabs_of(data, &Tab::ALL, width, height)
}

fn tabs_of(data: fn() -> Snapshot, tabs: &[Tab], width: u16, height: u16) -> String {
    tabs.iter()
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

/// Names, hosts and messages in Japanese and with emoji: each takes two cells per character.
fn wide_characters() -> Snapshot {
    let mut data = fixture();
    data.status = parse(json!({
        "version": "0.1.0", "open_advice": 0, "jobs": {"queued": 1, "running": 1, "failed": 1},
        "devices": [
            {"info": {"id": "ビルドサーバー-東京-01", "os": "linux"}, "online": true, "last_seen_ms": NOW - 3_000,
             "metrics": {"cpu_busy_pct": 12.0, "load_avg_1m": 0.8, "mem_free_pct": 60.0, "mem_available_bytes": 9_000_000_000u64},
             "guard": {"action": {"kind": "run", "slots": 1, "mem_mib": 2048}, "reasons": ["run: すべて正常です"]},
             "running": [{"id": 52, "name": "🚀 deploy-preview", "status": "running"}],
             "capacity": {"state": "cannot_run", "slots_used": 1, "slots_allowed": 2, "mem_used_mib": 512,
                          "mem_allowed_mib": 2560, "why": "1 queued job cannot run here: 日本語ツールがありません"}},
            {"info": {"id": "mac-🚀-dev", "os": "macos"}, "online": true, "last_seen_ms": NOW - 2_000,
             "guard": {"action": {"kind": "pause"}, "reasons": ["pause: バッテリー残量が 28% で 40% を下回っています"]},
             "capacity": {"state": "held", "slots_used": 0, "mem_used_mib": 0, "why": "held: battery"}}
        ],
    }));
    data.jobs = parse(json!([
        job(
            53,
            "日本語のレポート生成ジョブ-nightly-まとめ",
            "queued",
            json!({})
        ),
        job(
            52,
            "🚀 deploy-preview-🔥",
            "running",
            json!({"device": "ビルドサーバー-東京-01", "started_ms": NOW - 5 * MIN})
        ),
        job(
            51,
            "ascii-control",
            "failed",
            json!({"device": "mac-🚀-dev", "attempts": 3, "last_error": "ビルドに失敗しました: 終了コード 2 (メモリ不足)"})
        ),
    ]));
    data.events = parse(json!([
        {"id": 9, "at_ms": NOW - MIN, "kind": "error", "device": "ビルドサーバー-東京-01", "job": 51,
         "message": "🔥 job 51 failed: ビルドに失敗しました。受信箱のメモで最後の 40 行を確認してください"},
        {"id": 8, "at_ms": NOW - 2 * MIN, "kind": "job_started", "job": 52, "message": "🚀 started"},
    ]));
    data.schedules = parse(json!([
        {"id": 1, "spec": {"name": "毎日のヘルスチェック-リポジトリ-A", "command": ["true"]}, "every": "1d",
         "created_ms": 0, "last_fired_ms": NOW - HOUR, "last_job": 51, "decision": {"kind": "wait", "until_ms": NOW + 5 * HOUR},
         "last_result": {"status": "failed", "exit_code": 2, "error": "メモリ不足で終了しました", "finished_ms": NOW - 3 * MIN}}
    ]));
    data
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
    // Typical host names (the 38 character one is for the narrow screens): roomy means nothing is dropped.
    let mut data = fixture();
    data.status.devices.remove(1);
    for tab in [Tab::Devices, Tab::Jobs] {
        let mut app = app_with(data.clone(), tab);
        out.push_str(&format!(
            "##### {} 120x40\n{}\n",
            tab.title(),
            screen(&mut app, 120, 40)
        ));
    }
    check("wide-120x40", &out);
}

#[test]
fn wide_characters_stay_inside_their_columns_at_80x24() {
    let tabs = [Tab::Devices, Tab::Jobs, Tab::Events, Tab::Schedules];
    check(
        "wide-characters-80x24",
        &tabs_of(wide_characters, &tabs, 80, 24),
    );
}

#[test]
fn reasons_pane_fits_80x24() {
    check("why-80x24", &why_screens(80, 24, true));
}

#[test]
fn reasons_pane_fits_120x40() {
    check("why-120x40", &why_screens(120, 40, false));
}

#[test]
fn the_report_panel_sits_above_the_devices_at_80x24() {
    let mut app = app_with(fixture(), Tab::Devices);
    app.report = parse(json!([
        {"label": "today", "jobs": {"ran": 2},
         "verdict": "today: 2 jobs ran (14 job-min); guard held 6.1 h, mostly memory pressure (5.2 h); queue was empty 1.3 h of idle time"},
        {"label": "last 7 days", "jobs": {"ran": 0},
         "verdict": "0 jobs ran in the last 7 days: held 38.0 h, mostly by memory pressure (31.2 h)"}
    ]));
    check(
        "report-80x24",
        &format!("##### Devices 80x24\n{}\n", screen(&mut app, 80, 24)),
    );
}
