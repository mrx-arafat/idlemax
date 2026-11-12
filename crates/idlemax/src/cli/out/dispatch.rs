//! Delivery: TOON views, tables and next-step hints of the dispatcher commands (`project`,
//! `mode`, `plan`).

use idlemax_core::dispatch::plan::{DevicePlan, Pick, Plan};
use idlemax_core::dispatch::project::ProjectView;
use idlemax_core::dispatch::task::Task;
use idlemax_core::dispatch::{Mode, ModeReply};

use super::toon::Node;
use crate::cli::print::table;

fn run(cmd: &str, to: &str) -> String {
    format!("Run `{cmd}` to {to}")
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// The project table; the device column only when more than one device is shown.
pub fn project_table(all: &[ProjectView], with_device: bool) -> String {
    if all.is_empty() {
        return "no projects yet: add one with `idlemax project add <path>`".into();
    }
    let mut header = vec!["NAME"];
    if with_device {
        header.push("DEVICE");
    }
    header.extend(["STATE", "OWNER", "BASE", "CHECK", "PATH"]);
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|v| {
            let p = &v.project;
            let mut row = vec![p.name.clone()];
            if with_device {
                row.push(p.device.clone());
            }
            row.extend([
                on_off(p.enabled).to_string(),
                if v.owner_active { "active" } else { "-" }.to_string(),
                p.settings.base_branch.clone().unwrap_or_else(|| "-".into()),
                words(&p.settings.check_command),
                p.path.clone(),
            ]);
            row
        })
        .collect();
    table(&header, &rows)
}

/// Each project as one TOON row.
pub fn project_list(all: &[ProjectView]) -> Node {
    let rows = all.iter().map(project_node).collect();
    Node::obj([("projects", Node::Arr(rows))])
}

fn project_node(v: &ProjectView) -> Node {
    let p = &v.project;
    Node::obj([
        ("id", Node::Int(p.id)),
        ("name", Node::str(&p.name)),
        ("device", Node::str(&p.device)),
        ("enabled", Node::Bool(p.enabled)),
        ("owner_active", Node::Bool(v.owner_active)),
        (
            "base",
            Node::str(p.settings.base_branch.clone().unwrap_or_else(|| "-".into())),
        ),
        ("check", Node::str(words(&p.settings.check_command))),
        ("path", Node::str(&p.path)),
    ])
}

/// A command result: what happened and the project after it.
pub fn project_result(result: &str, v: &ProjectView) -> Node {
    let mut fields = vec![
        ("result".to_string(), Node::str(result)),
        ("project".to_string(), project_node(v)),
    ];
    fields.extend(
        v.warning
            .clone()
            .map(|w| ("warning".to_string(), Node::Str(w))),
    );
    Node::Obj(fields)
}

/// One line for a command result, with the allowlist warning under it.
pub fn project_line(result: &str, v: &ProjectView) -> String {
    let p = &v.project;
    let mut line = format!("project {} on {}: {result}", p.name, p.device);
    if let Some(w) = &v.warning {
        line.push_str(&format!("\nwarning: {w}"));
    }
    line
}

pub fn projects_hints(all: &[ProjectView]) -> Vec<String> {
    if all.is_empty() {
        return vec![run("idlemax project add <path>", "add a git checkout")];
    }
    vec![
        run(
            "idlemax project on <name>",
            "let the dispatcher plan work there",
        ),
        run(
            "idlemax project set <name> check -- <command>",
            "give a project its focused check",
        ),
    ]
}

pub fn project_hints(v: &ProjectView) -> Vec<String> {
    let p = &v.project;
    if !p.enabled {
        return vec![run(
            &format!("idlemax project on {}", p.name),
            "switch it on",
        )];
    }
    let mut hints = Vec::new();
    if p.settings.base_branch.is_none() {
        hints.push(run(
            &format!("idlemax project set {} base <branch>", p.name),
            "set its base branch",
        ));
    }
    if p.settings.check_command.is_empty() {
        hints.push(run(
            &format!("idlemax project set {} check -- <command>", p.name),
            "set its focused check",
        ));
    }
    hints
}

/// argv as one line, or "-" for none.
pub fn words(argv: &[String]) -> String {
    if argv.is_empty() {
        "-".into()
    } else {
        argv.join(" ")
    }
}

pub fn mode_result(result: &str, reply: &ModeReply) -> Node {
    Node::obj([
        ("result", Node::str(result)),
        ("mode", Node::str(reply.mode.as_str())),
        ("before", Node::str(reply.before.as_str())),
        ("changed", Node::Bool(reply.changed)),
    ])
}

pub fn mode_hints(mode: Mode) -> Vec<String> {
    match mode {
        Mode::Off => vec![run("idlemax mode propose", "let the dispatcher plan again")],
        Mode::Propose => vec![run("idlemax plan", "see what it would launch and why")],
    }
}

/// The plan as people read it: mode, who picked, per device the room, the picks and the
/// empty slots, then the tasks not judged, the "needs you" list and the notes.
pub fn plan_text(plan: &Plan) -> String {
    let mut out = vec![match plan.mode {
        Mode::Off => "dispatcher: off".to_string(),
        Mode::Propose => "dispatcher: propose (nothing is launched)".to_string(),
    }];
    out.push(format!("picked by: {}", plan.picked_by));
    for d in &plan.devices {
        out.push(device_line(d));
        for p in &d.picks {
            out.extend(pick_lines(p));
        }
        if let Some(e) = &d.empty {
            out.push(format!("  {} empty slots: {}", e.count, e.reason));
        }
    }
    let task_line = |t: &Task| format!("  {}  {}  {}", t.project, t.source.as_str(), t.title);
    if !plan.not_judged.is_empty() {
        out.push("not judged by the orchestrator:".into());
        out.extend(plan.not_judged.iter().map(task_line));
    }
    if !plan.needs_you.is_empty() {
        out.push("needs you:".into());
        out.extend(plan.needs_you.iter().map(|t| {
            let why = t.needs_you.as_deref().unwrap_or_default();
            format!("{}: {why}", task_line(t))
        }));
    }
    if !plan.notes.is_empty() {
        out.push("notes:".into());
        out.extend(plan.notes.iter().map(|n| format!("  {n}")));
    }
    out.join("\n")
}

fn device_line(d: &DevicePlan) -> String {
    let r = &d.room;
    match &d.held {
        Some(h) => format!("device {}: held: {h}", d.device),
        None => format!(
            "device {}: {}, {} open slots: {}",
            d.device,
            r.state.as_str(),
            r.open_slots,
            r.reason
        ),
    }
}

fn pick_lines(p: &Pick) -> Vec<String> {
    let source = p.task.split(':').next().unwrap_or_default();
    let provider = serde_json::to_value(p.provider)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    vec![
        format!("  pick {}  {source}  {}", p.project, p.title),
        format!("    {provider}  {}  {}", p.account, p.model),
        format!("    scope: {}", words(&p.scope_paths)),
        format!("    why: {}", p.reason),
    ]
}

pub fn plan_node(plan: &Plan) -> Result<Node, serde_json::Error> {
    Node::of(plan)
}

pub fn refresh_hint() -> String {
    run(
        "idlemax plan",
        "see the picks once the orchestrator pass has finished",
    )
}

pub fn plan_hints(plan: &Plan) -> Vec<String> {
    if plan.mode == Mode::Off {
        return vec![run("idlemax mode propose", "let the dispatcher plan again")];
    }
    let mut hints = Vec::new();
    if plan.devices.iter().all(|d| d.picks.is_empty()) && plan.not_judged.is_empty() {
        hints.push(run(
            "idlemax project list",
            "see the projects and switch one on",
        ));
    }
    hints
}
