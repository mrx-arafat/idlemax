//! Adapter: `idlemax hooks status`. Reads each agent's hook file and says whether the hook is
//! there and whether it still points at something that runs.

use std::path::Path;

use anyhow::{Context, Result};
use idlemax_core::install::hooks::{self, HookState};
use idlemax_core::install::quote::read_shell_word;
use idlemax_core::install::{codex, opencode};

use super::agents::{new_report, refused, report};
use super::places::{selected, Agent, Places};
use super::{AgentReport, Outcome, Report, Which};
use crate::cli::files::read_optional;

// ---- status --------------------------------------------------------------------------------

pub(super) fn run(which: &Which) -> Result<Report> {
    let places = Places::from_env()?;
    let chosen: Vec<Agent> = if which.claude || which.codex || which.opencode {
        selected(which, &places)
            .into_iter()
            .map(|(a, _)| a)
            .collect()
    } else {
        Agent::ALL.to_vec()
    };
    let agents = chosen
        .into_iter()
        .map(|agent| {
            let file = places.file(agent);
            if !places.dir(agent).is_dir() {
                return new_report(agent, file, Outcome::NotFound);
            }
            status_of(agent, &places, &file).unwrap_or_else(|e| refused(agent, file.clone(), &e))
        })
        .collect();
    Ok(report("status", false, agents, Vec::new()))
}

fn status_of(agent: Agent, places: &Places, file: &Path) -> Result<AgentReport> {
    let text = read_optional(file)?;
    let command = match agent {
        Agent::OpenCode => text
            .as_deref()
            .filter(|t| opencode::is_managed(t))
            .and_then(opencode::program_of),
        _ => match hooks::inspect(text.as_deref()).with_context(|| file.display().to_string())? {
            HookState::Installed { command } => Some(command),
            _ => None,
        },
    };
    let mut r = new_report(agent, file.to_path_buf(), Outcome::NotInstalled);
    let Some(command) = command else {
        return Ok(r);
    };
    r.outcome = Outcome::Installed;
    if let Some(problem) = binary_problem(&command, agent == Agent::OpenCode) {
        r.outcome = Outcome::Stale;
        r.notes.push(problem);
    }
    if agent == Agent::Codex
        && !codex::hooks_enabled(read_optional(&places.codex_config())?.as_deref())
    {
        r.outcome = Outcome::Stale;
        r.notes
            .push("[features] hooks is off in config.toml, so Codex ignores hooks.json".into());
    }
    Ok(r)
}

/// Why the program a hook runs would not start, or None. `program_only`: the text is the program.
fn binary_problem(command: &str, program_only: bool) -> Option<String> {
    let program = if program_only {
        command.to_string()
    } else {
        read_shell_word(command)?.0
    };
    let found = if program.contains('/') {
        Path::new(&program).is_file()
    } else {
        std::env::var_os("PATH").is_some_and(|p| hooks::find_on_path(&p, &program).is_some())
    };
    (!found).then(|| format!("{program} was not found; run `idlemax hooks install` to repair it"))
}
