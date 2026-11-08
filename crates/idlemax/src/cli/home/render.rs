//! Delivery: the home view (`idlemax` with no arguments) and the SessionStart hook summary.
//! One render function with two modes over a plain struct built from the API replies.
//! The text is TOON-shaped (`key: value`, `name[N]{fields}:` tables, `help[N]:`).

use std::path::Path;

use idlemax_core::protocol::{ClusterStatus, JobCounts};
use idlemax_core::quota::QuotaView;
use serde::Serialize;

use crate::cli::rows::guard_summary;

/// What the tool is, in one sentence.
pub const DESCRIPTION: &str =
    "Run useful jobs on spare device capacity and spend leftover AI quota before it resets";

/// (command, what it does): the static part of the help, also the body of the generated skill.
pub const COMMANDS: &[(&str, &str)] = &[
    (
        "idlemax status",
        "see the cluster, its devices and the guard verdict of each",
    ),
    (
        "idlemax jobs --status <status>",
        "list jobs, for example failed or parked",
    ),
    (
        "idlemax job add <spec.toml>",
        "queue a job from a TOML spec, or a command after `--`",
    ),
    (
        "idlemax why <id>",
        "see why a job is not running and what to change",
    ),
    (
        "idlemax quota",
        "see AI account windows and the headroom to spend",
    ),
    ("idlemax advise", "review the advisor's recommendations"),
    (
        "idlemax pause",
        "hold jobs everywhere, or on one `--device`, `--account` or `--job`",
    ),
    ("idlemax stop-all", "kill running jobs and hold new ones"),
    (
        "idlemax doctor",
        "check this device's setup and get the fix for anything wrong",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `idlemax` with no arguments.
    Full,
    /// `idlemax hooks summary`: loaded into every agent session, so as small as it can be.
    Hook,
}

#[derive(Debug, Serialize)]
pub struct Home {
    pub bin: String,
    pub description: &'static str,
    pub coordinator: String,
    #[serde(flatten)]
    pub state: State,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum State {
    Up(Live),
    Down { reason: Reason, detail: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No API token yet: `idlemax init` has not run on this device.
    NotSetUp,
    Unreachable,
    Failed,
}

#[derive(Debug, Serialize)]
pub struct Live {
    pub version: String,
    pub devices: Vec<DeviceLine>,
    pub jobs: JobCounts,
    pub open_advice: u64,
    pub quota: Vec<QuotaLine>,
}

#[derive(Debug, Serialize)]
pub struct DeviceLine {
    pub id: String,
    /// online, offline, paused or local-paused.
    pub state: String,
    /// The guard verdict, such as `run 2` or `pause: disk free 4 GiB`; `-` before the first heartbeat.
    pub guard: String,
}

#[derive(Debug, Serialize)]
pub struct QuotaLine {
    pub account: String,
    pub spendable: bool,
}

/// The live part of the view from `GET /v1/status` and `GET /v1/quota`.
pub fn live(status: &ClusterStatus, quota: &[QuotaView]) -> Live {
    let devices = status
        .devices
        .iter()
        .map(|d| DeviceLine {
            id: d.info.id.clone(),
            state: match (d.online, d.paused, d.local_paused) {
                (false, _, _) => "offline",
                (true, true, _) => "paused",
                (true, false, true) => "local-paused",
                (true, false, false) => "online",
            }
            .into(),
            guard: d.guard.as_ref().map_or_else(|| "-".into(), guard_summary),
        })
        .collect();
    let mut accounts: Vec<QuotaLine> = Vec::new();
    for q in quota {
        // Every window row of an account carries the account's own verdict.
        if accounts.iter().all(|a| a.account != q.account) {
            accounts.push(QuotaLine {
                account: q.account.clone(),
                spendable: q.spendable && !status.paused_accounts.contains(&q.account),
            });
        }
    }
    Live {
        version: status.version.clone(),
        devices,
        jobs: status.jobs.clone(),
        open_advice: status.open_advice,
        quota: accounts,
    }
}

/// `path` with the user's home directory shown as `~`.
pub fn collapse_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

pub fn render(h: &Home, mode: Mode) -> String {
    let mut out = Vec::new();
    match (&h.state, mode) {
        (State::Up(live), Mode::Full) => {
            out.push(format!("bin: {}", cell(&h.bin)));
            out.push(format!("description: {}", h.description));
            out.push(format!("coordinator: {}", cell(&h.coordinator)));
            out.extend(devices(&live.devices));
            out.push(format!("jobs: {}", jobs_line(&live.jobs, true)));
            out.push(format!("advice: {} open", live.open_advice));
            out.extend(quota(&live.quota));
            out.extend(help(&up_steps(live)));
        }
        (State::Up(live), Mode::Hook) => {
            out.extend(devices(&live.devices));
            out.push(format!("jobs: {}", jobs_line(&live.jobs, false)));
            if live.open_advice > 0 {
                out.push(format!("advice: {} open", live.open_advice));
            }
            out.extend(quota(&live.quota));
            out.extend(help(&["Run `idlemax` for the full view".into()]));
        }
        (State::Down { reason, detail }, Mode::Full) => {
            out.push(format!("bin: {}", cell(&h.bin)));
            out.push(format!("description: {}", h.description));
            out.push(down_line(h, *reason, detail));
            out.extend(help(&down_steps(*reason)));
        }
        (State::Down { reason, detail }, Mode::Hook) => {
            let fix = if *reason == Reason::NotSetUp {
                "init"
            } else {
                "doctor"
            };
            out.push(format!(
                "{} (run `idlemax {fix}`)",
                down_line(h, *reason, detail)
            ));
        }
    }
    out.join("\n")
}

fn down_line(h: &Home, reason: Reason, detail: &str) -> String {
    match reason {
        Reason::NotSetUp => "idlemax: not set up on this device".into(),
        Reason::Unreachable => format!("idlemax: coordinator {} is not reachable", h.coordinator),
        Reason::Failed => format!("idlemax: coordinator {}: {detail}", h.coordinator),
    }
}

fn devices(list: &[DeviceLine]) -> Vec<String> {
    if list.is_empty() {
        return vec!["devices: none have sent a heartbeat yet".into()];
    }
    let mut out = vec![format!("devices[{}]{{id,state,guard}}:", list.len())];
    out.extend(
        list.iter()
            .map(|d| format!("  {},{},{}", cell(&d.id), d.state, cell(&d.guard))),
    );
    out
}

/// Running plus leased jobs; `all` also names the zero counts.
fn jobs_line(j: &JobCounts, all: bool) -> String {
    let rows = [
        (j.running + j.leased, "running", true),
        (j.queued, "queued", true),
        (j.paused, "paused", all),
        (j.failed, "failed", all),
        (j.parked, "parked", all),
    ];
    let parts: Vec<String> = rows
        .iter()
        .filter(|(n, _, always)| *always || *n > 0)
        .map(|(n, word, _)| format!("{n} {word}"))
        .collect();
    parts.join(" ")
}

fn quota(list: &[QuotaLine]) -> Vec<String> {
    if list.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("quota[{}]{{account,spendable}}:", list.len())];
    out.extend(
        list.iter()
            .map(|q| format!("  {},{}", cell(&q.account), q.spendable)),
    );
    out
}

/// Next steps that follow from the live state; the static commands fill the rest.
fn up_steps(live: &Live) -> Vec<String> {
    let j = &live.jobs;
    let mut steps = Vec::new();
    if live.devices.is_empty() {
        steps.push("Run `idlemax agent` on a device so it can take jobs".to_string());
    }
    if j.failed > 0 {
        steps.push(format!(
            "Run `idlemax jobs --status failed` to see the {} failed",
            j.failed
        ));
    }
    if j.parked > 0 {
        steps.push(format!(
            "Run `idlemax jobs --status parked` to see the {} parked",
            j.parked
        ));
    }
    if j.queued > 0 && j.running + j.leased == 0 {
        steps.push("Run `idlemax why <id>` to see why a queued job is not running".into());
    }
    if live.open_advice > 0 {
        steps.push(format!(
            "Run `idlemax advise` to review {} open",
            live.open_advice
        ));
    }
    for (cmd, what) in COMMANDS {
        let verb: Vec<&str> = cmd.split_whitespace().take(2).collect();
        let verb = verb.join(" ");
        let taken =
            |s: &String| s.contains(&format!("`{verb} ")) || s.contains(&format!("`{verb}`"));
        if steps.len() < 4 && !steps.iter().any(taken) {
            steps.push(format!("Run `{cmd}` to {what}"));
        }
    }
    steps.truncate(4);
    steps
}

fn down_steps(reason: Reason) -> Vec<String> {
    let first = match reason {
        Reason::NotSetUp => "Run `idlemax init` to create the config and the API token",
        Reason::Unreachable => "Run `idlemax coord` to start a coordinator on this machine",
        Reason::Failed => "Run `idlemax doctor` to find what is wrong",
    };
    let mut steps = vec![first.to_string()];
    if reason != Reason::Failed {
        steps.push("Run `idlemax doctor` to find what is wrong".into());
    }
    if reason == Reason::Unreachable {
        steps.push("Run `idlemax local status` to ask this device's agent directly".into());
    }
    steps
}

fn help(steps: &[String]) -> Vec<String> {
    match steps {
        [one] => vec![format!("help[1]: {one}")],
        many => {
            let mut out = vec![format!("help[{}]:", many.len())];
            out.extend(many.iter().map(|s| format!("  {s}")));
            out
        }
    }
}

/// A TOON value: quoted when a bare one would be ambiguous.
fn cell(s: &str) -> String {
    let ambiguous = s.is_empty()
        || s != s.trim()
        || s.contains([',', ':', '"', '\\', '\n'])
        || matches!(s, "true" | "false" | "null")
        || s.parse::<f64>().is_ok();
    if !ambiguous {
        return s.to_string();
    }
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}
