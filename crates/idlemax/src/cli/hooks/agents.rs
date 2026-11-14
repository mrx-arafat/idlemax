//! Adapter: what `idlemax hooks install` and `uninstall` do to each agent's files. Every plan
//! is made before anything is written, a file that cannot be understood is reported and left
//! alone, and one agent failing never blocks another.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use idlemax_core::install::diff::line_diff;
use idlemax_core::install::hooks::{self, HookState};
use idlemax_core::install::service::bin_warning;
use idlemax_core::install::{codex, opencode};

use super::places::{selected, Agent, Places};
use super::{AgentReport, Outcome, Report, Which};
use crate::cli::files::{backup_path, read_optional, save, write_atomic};

pub(super) fn report(
    action: &'static str,
    dry_run: bool,
    agents: Vec<AgentReport>,
    skipped: Vec<&'static str>,
) -> Report {
    Report {
        action,
        dry_run,
        agents,
        skipped,
    }
}

/// Agents nothing named and whose directory is missing; empty when the user named any.
pub(super) fn left_out(which: &Which, places: &Places) -> Vec<&'static str> {
    if which.claude || which.codex || which.opencode {
        return Vec::new();
    }
    Agent::ALL
        .into_iter()
        .filter(|a| !places.found(*a))
        .map(Agent::name)
        .collect()
}

pub(super) fn new_report(agent: Agent, path: PathBuf, outcome: Outcome) -> AgentReport {
    AgentReport {
        agent: agent.name(),
        outcome,
        path,
        backup: None,
        notes: Vec::new(),
        diff: String::new(),
    }
}

pub(super) fn refused(agent: Agent, path: PathBuf, why: &anyhow::Error) -> AgentReport {
    let mut r = new_report(agent, path, Outcome::Refused);
    r.notes.push(format!("{why:#}"));
    r
}

/// The command a hook runs: the bare name when PATH resolves it to this executable, else the
/// absolute path; plus the bare program and the state folder for the OpenCode plugin and a
/// warning when the executable sits where a rebuild would replace it.
fn command() -> Result<(String, Hook, Option<String>)> {
    let exe = std::fs::canonicalize(std::env::current_exe()?).context("finding this executable")?;
    if exe.to_str().is_none() {
        bail!("{} is not valid UTF-8", exe.display());
    }
    let on_path = std::env::var_os("PATH")
        .and_then(|p| hooks::find_on_path(&p, hooks::BINARY))
        .and_then(|p| std::fs::canonicalize(p).ok());
    let program = hooks::program_for(&exe, on_path.as_deref());
    let warning = bin_warning(&exe).map(|_| {
        format!(
            "warning: the hook runs {}, inside a cargo target directory that a rebuild or `cargo clean` replaces; install idlemax (`cargo install --path crates/idlemax`) and run `idlemax hooks install` again",
            exe.display()
        )
    });
    let home = custom_home()?;
    let command = hooks::hook_command(&program, home.as_deref());
    Ok((command, Hook { program, home }, warning))
}

/// What the OpenCode plugin runs: the program, and the state folder when it is not the default.
pub(super) struct Hook {
    pub(super) program: String,
    pub(super) home: Option<String>,
}

/// The state folder from `IDLEMAX_HOME`, which a new agent session would not inherit; None for
/// the default folder, which the summary finds by itself.
fn custom_home() -> Result<Option<String>> {
    if std::env::var_os(idlemax_core::paths::HOME_ENV).is_none() {
        return Ok(None);
    }
    let paths = crate::state::paths_from_env()?;
    let root = std::fs::canonicalize(paths.root()).unwrap_or_else(|_| paths.root().to_path_buf());
    match root.to_str() {
        Some(dir) => Ok(Some(dir.to_string())),
        None => bail!("{} is not valid UTF-8", root.display()),
    }
}

// ---- install -------------------------------------------------------------------------------

pub fn install(which: &Which, dry_run: bool) -> Result<Report> {
    let places = Places::from_env()?;
    let (command, hook, warning) = command()?;
    let chosen = selected(which, &places);
    if chosen.is_empty() {
        return Err(crate::cli::CliError::runtime(
            format!(
                "no supported agent found ({}, {} or {})",
                places.claude.display(),
                places.codex.display(),
                places.opencode.display()
            ),
            "idlemax hooks install --claude (or --codex, --opencode) to set one up anyway",
        )
        .into());
    }
    let mut agents: Vec<AgentReport> = chosen
        .into_iter()
        .map(|(agent, _)| {
            let result = match agent {
                Agent::Claude => json_install(agent, &places.file(agent), &command, dry_run),
                Agent::Codex => codex_install(&places, &command, dry_run),
                Agent::OpenCode => super::opencode_migration::install(&places, &hook, dry_run),
            };
            result.unwrap_or_else(|e| refused(agent, places.file(agent), &e))
        })
        .collect();
    if let (Some(warning), Some(first)) = (warning, agents.first_mut()) {
        first.notes.push(warning);
    }
    Ok(report("install", dry_run, agents, left_out(which, &places)))
}

/// What a text edit did, from the file before and after.
fn outcome(before: Option<&str>, after: &str, was_installed: bool) -> Outcome {
    match (before == Some(after), was_installed) {
        (true, _) => Outcome::Unchanged,
        (false, true) => Outcome::Repaired,
        (false, false) => Outcome::Installed,
    }
}

fn diff(path: &Path, before: Option<&str>, after: &str) -> String {
    let shown = super::collapse_home(path, dirs::home_dir().as_deref());
    format!("{shown}:\n{}", line_diff(before.unwrap_or(""), after, 2))
}

fn json_install(agent: Agent, file: &Path, command: &str, dry_run: bool) -> Result<AgentReport> {
    let ctx = || file.display().to_string();
    let before = read_optional(file)?;
    let was = matches!(
        hooks::inspect(before.as_deref()).with_context(ctx)?,
        HookState::Installed { .. }
    );
    let after = hooks::plan_install(before.as_deref(), command).with_context(ctx)?;
    let mut r = new_report(
        agent,
        file.to_path_buf(),
        outcome(before.as_deref(), &after, was),
    );
    if r.outcome != Outcome::Unchanged {
        r.diff = diff(file, before.as_deref(), &after);
        if !dry_run {
            r.backup = save(file, before.as_deref(), &after)?;
        }
    }
    Ok(r)
}

fn codex_install(places: &Places, command: &str, dry_run: bool) -> Result<AgentReport> {
    let (file, config) = (places.file(Agent::Codex), places.codex_config());
    let ctx = |p: &Path| p.display().to_string();
    // Both plans first: a bad file refuses the whole agent before anything is written.
    let before = read_optional(&file)?;
    let was = matches!(
        hooks::inspect(before.as_deref()).with_context(|| ctx(&file))?,
        HookState::Installed { .. }
    );
    let after = hooks::plan_install(before.as_deref(), command).with_context(|| ctx(&file))?;
    let cfg_before = read_optional(&config)?;
    let cfg_after = codex::plan_enable(cfg_before.as_deref()).with_context(|| ctx(&config))?;
    let mut r = new_report(
        Agent::Codex,
        file.clone(),
        outcome(before.as_deref(), &after, was),
    );
    if cfg_before.as_deref() != Some(&cfg_after) {
        let shown = super::collapse_home(&config, dirs::home_dir().as_deref());
        r.notes.push(format!("enabled [features] hooks in {shown}"));
        r.diff = diff(&config, cfg_before.as_deref(), &cfg_after);
        if r.outcome == Outcome::Unchanged {
            r.outcome = Outcome::Repaired;
        }
        if !dry_run {
            save(&config, cfg_before.as_deref(), &cfg_after)?;
        }
    }
    if before.as_deref() != Some(&after) {
        r.diff.push_str(&diff(&file, before.as_deref(), &after));
        if !dry_run {
            r.backup = save(&file, before.as_deref(), &after)?;
        }
    }
    Ok(r)
}

pub(super) fn plugin_install(file: &Path, hook: &Hook, dry_run: bool) -> Result<AgentReport> {
    let before = read_optional(file)?;
    if before.as_deref().is_some_and(|t| !opencode::is_managed(t)) {
        bail!(
            "{} exists and idlemax did not write it; move it away, then run this again",
            file.display()
        );
    }
    let after = opencode::plugin_source(&hook.program, hook.home.as_deref());
    let r = new_report(
        Agent::OpenCode,
        file.to_path_buf(),
        outcome(before.as_deref(), &after, before.is_some()),
    );
    if r.outcome != Outcome::Unchanged && !dry_run {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write_atomic(file, after.as_bytes(), None)
            .with_context(|| format!("writing {}", file.display()))?;
    }
    Ok(r)
}

// ---- uninstall -----------------------------------------------------------------------------

pub fn uninstall(which: &Which, dry_run: bool) -> Result<Report> {
    let places = Places::from_env()?;
    let agents = selected(which, &places)
        .into_iter()
        .map(|(agent, _)| {
            let file = places.file(agent);
            let result = match agent {
                Agent::OpenCode => super::opencode_migration::uninstall(&places, dry_run),
                _ => json_remove(agent, &file, dry_run),
            };
            let mut r = result.unwrap_or_else(|e| refused(agent, file, &e));
            if agent == Agent::Codex && r.outcome == Outcome::Removed {
                r.notes.push(
                    "[features] hooks stays on in config.toml; other hooks may use it".into(),
                );
            }
            r
        })
        .collect();
    Ok(report(
        "uninstall",
        dry_run,
        agents,
        left_out(which, &places),
    ))
}

fn json_remove(agent: Agent, file: &Path, dry_run: bool) -> Result<AgentReport> {
    let before = read_optional(file)?;
    let Some(text) = before.as_deref() else {
        return Ok(new_report(agent, file.to_path_buf(), Outcome::NotInstalled));
    };
    let backup = read_optional(&backup_path(file))?;
    let after = hooks::plan_uninstall(text, backup.as_deref())
        .with_context(|| file.display().to_string())?;
    if after == text {
        return Ok(new_report(agent, file.to_path_buf(), Outcome::NotInstalled));
    }
    let mut r = new_report(agent, file.to_path_buf(), Outcome::Removed);
    r.diff = diff(file, before.as_deref(), &after);
    if !dry_run {
        r.backup = save(file, before.as_deref(), &after)?;
    }
    Ok(r)
}

pub(super) fn plugin_remove(file: &Path, dry_run: bool) -> Result<AgentReport> {
    let text = read_optional(file)?;
    let mut r = new_report(Agent::OpenCode, file.to_path_buf(), Outcome::NotInstalled);
    match text.as_deref() {
        Some(t) if opencode::is_managed(t) => {
            r.outcome = Outcome::Removed;
            if !dry_run {
                std::fs::remove_file(file)
                    .with_context(|| format!("removing {}", file.display()))?;
            }
        }
        Some(_) => r
            .notes
            .push("this file was not written by idlemax, so it stays".into()),
        None => {}
    }
    Ok(r)
}
