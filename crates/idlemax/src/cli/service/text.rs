//! Delivery: plain-text rendering of the `idlemax service` reports.

use idlemax_core::install::service::ServiceStatus;

use super::{InstallReport, UninstallReport};
use crate::cli::print::table;

pub(super) fn install_text(r: &InstallReport) -> String {
    let name = r.kind.name();
    let mut out = String::new();
    if r.dry_run {
        out.push_str(&format!(
            "dry run, nothing written or started\nfile: {}\n----\n{}----\n",
            r.file.display(),
            r.content
        ));
        out.push_str("then it would run:\n");
        r.commands
            .iter()
            .for_each(|c| out.push_str(&format!("  {c}\n")));
    } else {
        out.push_str(&format!(
            "{name} installed and started\nfile: {}\ncheck it with `idlemax service status`\n",
            r.file.display()
        ));
    }
    r.notes.iter().for_each(|n| out.push_str(&format!("{n}\n")));
    out.trim_end().to_string()
}

pub(super) fn uninstall_text(r: &UninstallReport) -> String {
    let name = r.kind.name();
    match (r.installed, r.dry_run) {
        (false, _) => format!("{name}: not installed"),
        (true, true) => {
            let cmds: Vec<String> = r.commands.iter().map(|c| format!("  {c}")).collect();
            format!(
                "dry run: would delete {} after\n{}",
                r.file.display(),
                cmds.join("\n")
            )
        }
        (true, false) => format!("{name} stopped and removed: {}", r.file.display()),
    }
}

pub(super) fn status_text(all: &[ServiceStatus]) -> String {
    let word = |v: Option<bool>| match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    };
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|s| {
            vec![
                s.kind.name().into(),
                word(Some(s.installed)).into(),
                word(s.loaded).into(),
                word(s.running).into(),
                s.file.display().to_string(),
            ]
        })
        .collect();
    let mut out = table(
        &["SERVICE", "INSTALLED", "LOADED", "RUNNING", "FILE"],
        &rows,
    );
    for s in all {
        if let Some(note) = &s.note {
            out.push_str(&format!("\n{}: {note}", s.kind.name()));
        }
    }
    out
}
