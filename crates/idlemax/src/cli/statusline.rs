//! Delivery: `idlemax statusline install|uninstall|status`. Reads and writes Claude Code's
//! `settings.json` through the pure edits in `idlemax_core::install::statusline`.
//! It backs the file up before every change, edits it through symlinks, and never rewrites
//! anything but the statusLine command.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use idlemax_core::install::diff::line_diff;
use idlemax_core::install::statusline::{
    inspect, plan_install, plan_uninstall, Wiring, Wrapper, WRAPPER_NAME, WRAPPER_SCRIPT,
};
use idlemax_core::Paths;
use serde::Serialize;

use super::files::{backup_path, read_optional, save, write_atomic};
use super::out::hints;
use super::Out;

#[derive(Args, Debug)]
pub struct StatuslineArgs {
    #[command(subcommand)]
    cmd: StatuslineCmd,
}

#[derive(Subcommand, Debug)]
enum StatuslineCmd {
    /// Wrap Claude's statusLine command so quota snapshots reach idlemax
    Install {
        /// Print the change as a diff without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Put the original statusLine command back
    Uninstall {
        /// Print the change as a diff without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Show how settings.json is wired and whether captures arrive
    Status,
}

/// Where everything lives, from `HOME`, `CLAUDE_CONFIG_DIR` and the state dir.
pub(super) struct Places {
    pub(super) settings: PathBuf,
    pub(super) wrapper: PathBuf,
    state: PathBuf,
    /// The state dir to pass on to the wrapper, set only when it is not the default.
    custom_home: Option<String>,
}

pub(super) fn places(paths: &Paths) -> Result<Places> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let claude_dir = match (var("CLAUDE_CONFIG_DIR"), var("HOME")) {
        (Some(dir), _) => PathBuf::from(dir),
        (None, Some(home)) => PathBuf::from(home).join(".claude"),
        (None, None) => {
            bail!("HOME is unset or empty; set CLAUDE_CONFIG_DIR to find settings.json")
        }
    };
    let state = std::path::absolute(paths.root())?;
    let utf8 = |p: &Path| {
        p.to_str()
            .map(str::to_string)
            .with_context(|| format!("{} is not valid UTF-8", p.display()))
    };
    Ok(Places {
        settings: std::path::absolute(claude_dir)?.join("settings.json"),
        wrapper: state.join("bin").join(WRAPPER_NAME),
        custom_home: var("IDLEMAX_HOME").map(|_| utf8(&state)).transpose()?,
        state,
    })
}

impl Places {
    fn wrapper_spec<'a>(&'a self, script: &'a str) -> Wrapper<'a> {
        Wrapper {
            script,
            idlemax_home: self.custom_home.as_deref(),
        }
    }

    fn capture_file(&self, home: Option<&str>) -> PathBuf {
        let root = home.map_or_else(|| self.state.clone(), PathBuf::from);
        root.join("agent/claude-statusline.json")
    }
}

#[derive(Serialize)]
pub(super) struct ChangeReport {
    dry_run: bool,
    pub(super) changed: bool,
    pub(super) settings: PathBuf,
    pub(super) backup: Option<PathBuf>,
    pub(super) wrapper: PathBuf,
    diff: String,
    pub(super) notes: Vec<String>,
}

#[derive(Serialize)]
struct StatusReport {
    settings: PathBuf,
    /// no_settings, no_statusline, not_wrapped or installed.
    state: &'static str,
    original_command: Option<String>,
    wrapper: Option<PathBuf>,
    wrapper_present: Option<bool>,
    capture: Option<PathBuf>,
    capture_age_secs: Option<u64>,
}

pub fn run(paths: &Paths, args: StatuslineArgs, out: &Out) -> Result<()> {
    let places = places(paths)?;
    match args.cmd {
        StatuslineCmd::Install { dry_run } => out.emit(
            &install(&places, dry_run)?,
            change_text,
            &hints::then("idlemax statusline status", "check how it is wired"),
        ),
        StatuslineCmd::Uninstall { dry_run } => out.emit(
            &uninstall(&places, dry_run)?,
            change_text,
            &hints::then("idlemax statusline status", "check how it is wired"),
        ),
        StatuslineCmd::Status => {
            let report = status(&places)?;
            let hints = match report.state {
                "installed" => Vec::new(),
                _ => hints::then(
                    "idlemax statusline install --dry-run",
                    "preview wiring the quota probe",
                ),
            };
            out.emit(&report, status_text, &hints)
        }
    }
}

pub(super) fn install(p: &Places, dry_run: bool) -> Result<ChangeReport> {
    let script = p
        .wrapper
        .to_str()
        .context("the wrapper path is not valid UTF-8")?;
    let before = read_optional(&p.settings)?;
    let after = plan_install(before.as_deref(), &p.wrapper_spec(script))
        .with_context(|| p.settings.display().to_string())?;
    let changed = before.as_deref() != Some(after.as_str());
    let mut notes = Vec::new();
    let wrapper_needed =
        match inspect(Some(&after)).with_context(|| p.settings.display().to_string())? {
            Wiring::Wrapped(w) if w.script != script => {
                if !Path::new(&w.script).exists() {
                    notes.push(format!("warning: the wrapper {} does not exist", w.script));
                }
                false
            }
            _ => true,
        };
    let mut backup = None;
    if !dry_run {
        if wrapper_needed {
            install_wrapper(&p.wrapper)?;
        }
        if changed {
            backup = save(&p.settings, before.as_deref(), &after)?;
        }
    }
    if !changed {
        notes.push("already installed, nothing to change".into());
    } else if !dry_run {
        notes.push(
            "open Claude Code once; `idlemax statusline status` shows when the first capture lands"
                .into(),
        );
    }
    Ok(ChangeReport {
        dry_run,
        changed,
        settings: p.settings.clone(),
        backup,
        wrapper: p.wrapper.clone(),
        diff: line_diff(before.as_deref().unwrap_or(""), &after, 2),
        notes,
    })
}

fn install_wrapper(path: &Path) -> Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|t| t == WRAPPER_SCRIPT) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        // `$IDLEMAX_HOME/bin`, so a missing state dir is created too: owner only.
        crate::state::create_private_dir(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    write_atomic(path, WRAPPER_SCRIPT.as_bytes(), Some(0o755))
        .with_context(|| format!("writing {}", path.display()))
}

fn uninstall(p: &Places, dry_run: bool) -> Result<ChangeReport> {
    let before = read_optional(&p.settings)?;
    let backup_text = read_optional(&backup_path(&p.settings))?;
    let after = match &before {
        Some(text) => plan_uninstall(text, backup_text.as_deref())
            .with_context(|| p.settings.display().to_string())?,
        None => String::new(),
    };
    let changed = before.as_deref().is_some_and(|b| b != after);
    let mut backup = None;
    if changed && !dry_run {
        backup = save(&p.settings, before.as_deref(), &after)?;
    }
    let note = if changed {
        "the original statusLine command is back".to_string()
    } else {
        format!(
            "nothing to undo: idlemax is not wired into {}",
            p.settings.display()
        )
    };
    Ok(ChangeReport {
        dry_run,
        changed,
        settings: p.settings.clone(),
        backup,
        wrapper: p.wrapper.clone(),
        diff: line_diff(before.as_deref().unwrap_or(""), &after, 2),
        notes: vec![note],
    })
}

fn status(p: &Places) -> Result<StatusReport> {
    let text = read_optional(&p.settings)?;
    let mut report = StatusReport {
        settings: p.settings.clone(),
        state: "no_settings",
        original_command: None,
        wrapper: None,
        wrapper_present: None,
        capture: None,
        capture_age_secs: None,
    };
    match inspect(text.as_deref()).with_context(|| p.settings.display().to_string())? {
        Wiring::NoSettings => {}
        Wiring::NoStatusLine => report.state = "no_statusline",
        Wiring::Plain { command } => {
            report.state = "not_wrapped";
            report.original_command = Some(command);
        }
        Wiring::Wrapped(w) => {
            report.state = "installed";
            report.wrapper_present = Some(Path::new(&w.script).exists());
            let capture = p.capture_file(w.idlemax_home.as_deref());
            report.capture_age_secs = std::fs::metadata(&capture)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .map(|d| d.as_secs());
            report.capture = Some(capture);
            report.wrapper = Some(PathBuf::from(w.script));
            report.original_command = w.original;
        }
    }
    Ok(report)
}

fn change_text(r: &ChangeReport) -> String {
    let mut out = String::new();
    if r.dry_run {
        out.push_str(&format!(
            "dry run, nothing written\nsettings: {}\n",
            r.settings.display()
        ));
        if r.changed {
            out.push_str(&format!(
                "wrapper script: {} (written on install)\n",
                r.wrapper.display()
            ));
        }
    } else if r.changed {
        out.push_str(&format!("updated {}\n", r.settings.display()));
        if let Some(b) = &r.backup {
            out.push_str(&format!("backup: {}\n", b.display()));
        }
    }
    out.push_str(&r.diff);
    r.notes.iter().for_each(|n| out.push_str(&format!("{n}\n")));
    out.trim_end().to_string()
}

fn status_text(r: &StatusReport) -> String {
    let mut out = format!("settings:  {}\n", r.settings.display());
    let state = match r.state {
        "no_settings" => "no settings.json yet; `idlemax statusline install` creates one",
        "no_statusline" => "no statusLine set; `idlemax statusline install` adds it",
        "not_wrapped" => "not installed; `idlemax statusline install` wraps the command",
        _ => "installed",
    };
    out.push_str(&format!("statusline: {state}\n"));
    if let Some(cmd) = &r.original_command {
        out.push_str(&format!("wraps:     {cmd}\n"));
    }
    if let (Some(w), Some(present)) = (&r.wrapper, r.wrapper_present) {
        let note = if present {
            "present"
        } else {
            "MISSING, run `idlemax statusline uninstall` then `install`"
        };
        out.push_str(&format!("wrapper:   {} ({note})\n", w.display()));
    }
    if let Some(capture) = &r.capture {
        let age = match r.capture_age_secs {
            Some(s) => format!("last written {s}s ago"),
            None => "not written yet; open Claude Code so it renders the statusline".into(),
        };
        out.push_str(&format!("capture:   {} ({age})\n", capture.display()));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests;
