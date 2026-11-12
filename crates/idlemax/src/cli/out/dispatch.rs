//! Delivery: TOON views, tables and next-step hints of the dispatcher commands (`project`,
//! `mode`, `plan`).

use idlemax_core::dispatch::project::ProjectView;

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
            Node::str(p.settings.base_branch.clone().unwrap_or_default()),
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
