//! Delivery: the plain-text report of `idlemax setup`: one block per step, then doctor's own
//! report, then one line saying what happened and what to do next.

use idlemax_core::doctor::Level;

use super::report::{SetupReport, Status, StepReport};
use crate::cli::out::toon::Node;
use crate::cli::print;
use crate::cli::rows::snake;

const STATUS_WIDTH: usize = 12;
const STEP_WIDTH: usize = 14;

pub fn render(r: &SetupReport) -> String {
    let mut out = String::new();
    if r.dry_run {
        out.push_str("dry run: nothing is written or run\n");
    }
    out.push_str(&format!("idlemax setup, role {}\n", r.role.name()));
    for step in &r.steps {
        out.push_str(&step_text(step, r.dry_run));
    }
    if let Some(doctor) = &r.doctor {
        out.push('\n');
        out.push_str(print::doctor(doctor).trim_end());
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&closing(r));
    out
}

fn status_word(s: Status) -> &'static str {
    match s {
        Status::Done => "done",
        Status::AlreadyDone => "already done",
        Status::Skipped => "skipped",
        Status::WouldDo => "would do",
        Status::Failed => "FAILED",
        Status::Problems => "problems",
        Status::Starting => "starting",
    }
}

fn step_text(step: &StepReport, dry_run: bool) -> String {
    let label = match step.which {
        Some(which) => format!("{} {which}", step.step),
        None => step.step.to_string(),
    };
    let pad = " ".repeat(STATUS_WIDTH + STEP_WIDTH + 4);
    let mut lines = step.detail.lines();
    let first = lines.next().unwrap_or_default();
    let mut out = format!(
        "{:<STATUS_WIDTH$}  {label:<STEP_WIDTH$}  {first}\n",
        status_word(step.status)
    );
    for line in lines {
        out.push_str(&format!("{pad}{line}\n"));
    }
    let (wrote, ran) = if dry_run {
        ("would write", "would run")
    } else {
        ("wrote", "ran")
    };
    for file in &step.files {
        out.push_str(&format!("{pad}{wrote}: {}\n", file.display()));
    }
    for run in &step.runs {
        out.push_str(&format!("{pad}{ran}: {run}\n"));
    }
    for note in &step.notes {
        out.push_str(&format!("{pad}note: {note}\n"));
    }
    out.push_str(&format!("{pad}by hand: {}\n", step.command));
    out
}

fn closing(r: &SetupReport) -> String {
    let role = r.role.name();
    if r.dry_run {
        return "dry run only: run the same command without --dry-run to apply it".into();
    }
    if r.steps.iter().any(|s| s.status == Status::Failed) {
        return "setup stopped at the step marked FAILED: fix it, then run `idlemax setup` \
                again (it is safe to repeat)"
            .into();
    }
    if r.steps.iter().any(|s| s.status == Status::Starting) {
        return "services are starting; run `idlemax doctor` to re-check".into();
    }
    let done = if r.changed {
        "setup is done"
    } else {
        "nothing to change"
    };
    if r.doctor.as_ref().is_some_and(|d| !d.healthy) {
        return format!(
            "{done}, but doctor found failing checks: apply the fix under each `fail` line, \
             then run `idlemax doctor`"
        );
    }
    if !r.changed {
        return format!("{done}: this device is already set up as {role}");
    }
    let next: Vec<String> = r.next.iter().map(|n| format!("`{n}`")).collect();
    format!("setup finished as {role}; next: {}", next.join(", "))
}

/// The TOON report: one row per step, the files and commands touched, then only the doctor
/// checks that are not ok. `ok` and `changed` come first, since they decide what to do next.
pub fn view(r: &SetupReport) -> Node {
    let label = |s: &StepReport| match s.which {
        Some(which) => format!("{} {which}", s.step),
        None => s.step.to_string(),
    };
    let rows = r.steps.iter().map(|s| {
        // The by-hand command matters only where the step did not just work.
        let by_hand = matches!(s.status, Status::Failed | Status::Skipped);
        Node::obj([
            ("step", Node::Str(label(s))),
            ("status", Node::str(snake(&s.status))),
            ("detail", Node::str(&s.detail)),
            (
                "by_hand",
                if by_hand {
                    Node::str(&s.command)
                } else {
                    Node::Null
                },
            ),
        ])
    });
    let mut fields = vec![
        ("role".to_string(), Node::str(r.role.name())),
        ("dry_run".to_string(), Node::Bool(r.dry_run)),
        ("ok".to_string(), Node::Bool(r.ok)),
        ("changed".to_string(), Node::Bool(r.changed)),
        ("steps".to_string(), Node::Arr(rows.collect())),
    ];
    let (wrote, ran) = if r.dry_run {
        ("would_write", "would_run")
    } else {
        ("wrote", "ran")
    };
    let listed = |pick: &dyn Fn(&StepReport) -> Vec<String>| {
        let all: Vec<Node> = r.steps.iter().flat_map(pick).map(Node::Str).collect();
        (!all.is_empty()).then_some(Node::Arr(all))
    };
    let extras: [(&str, Option<Node>); 3] = [
        (
            wrote,
            listed(&|s| s.files.iter().map(|f| f.display().to_string()).collect()),
        ),
        (ran, listed(&|s| s.runs.clone())),
        (
            "notes",
            listed(&|s| {
                s.notes
                    .iter()
                    .map(|n| format!("{}: {n}", label(s)))
                    .collect()
            }),
        ),
    ];
    fields.extend(
        extras
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k.to_string(), v))),
    );
    if let Some(d) = &r.doctor {
        let problems: Vec<Node> = d
            .checks
            .iter()
            .filter(|c| c.level != Level::Ok)
            .map(|c| {
                Node::obj([
                    ("id", Node::str(&c.id)),
                    ("level", Node::str(snake(&c.level))),
                    ("detail", Node::str(&c.detail)),
                    ("fix", c.fix.as_deref().map_or(Node::Null, Node::str)),
                ])
            })
            .collect();
        let doctor = if problems.is_empty() {
            Node::str(format!("{} checks ok", d.counts.ok))
        } else {
            Node::Arr(problems)
        };
        fields.push(("doctor".to_string(), doctor));
    }
    fields.push(("result".to_string(), Node::Str(closing(r))));
    Node::Obj(fields)
}
