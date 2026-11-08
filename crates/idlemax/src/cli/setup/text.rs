//! Delivery: the plain-text report of `idlemax setup`: one block per step, then doctor's own
//! report, then one line saying what happened and what to do next.

use super::report::{SetupReport, Status, StepReport};
use crate::cli::print;

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
