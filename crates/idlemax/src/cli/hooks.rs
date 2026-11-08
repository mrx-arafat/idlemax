//! Delivery: `idlemax hooks install|uninstall|status|summary`. Registers the compact session
//! summary with Claude Code, Codex and OpenCode, only when asked, and only ever edits its own
//! entry. The file work is in `agents`; this file holds the arguments, the report and its text.

mod agents;
mod places;
mod status;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use serde::Serialize;

use super::home::{self, collapse_home, Mode};

#[derive(Args, Debug)]
pub struct HooksArgs {
    #[command(subcommand)]
    cmd: HooksCmd,
}

#[derive(Subcommand, Debug)]
enum HooksCmd {
    /// Run the summary at the start of every Claude Code, Codex and OpenCode session
    Install(Change),
    /// Remove the summary again; every other hook stays
    Uninstall(Change),
    /// Show where the hook is installed and whether it still points at a working binary
    Status(Which),
    /// Print the compact summary a session hook loads (always exits 0)
    Summary,
}

/// Which agents to act on. With no flag, every agent whose config directory exists.
#[derive(Args, Debug, Clone, Copy)]
pub struct Which {
    /// Claude Code (~/.claude/settings.json)
    #[arg(long)]
    claude: bool,
    /// Codex (~/.codex/hooks.json and config.toml)
    #[arg(long)]
    codex: bool,
    /// OpenCode (~/.config/opencode/plugins/idlemax.js)
    #[arg(long)]
    opencode: bool,
}

#[derive(Args, Debug)]
struct Change {
    #[command(flatten)]
    which: Which,
    /// Print the change as a diff without writing anything
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Installed,
    Repaired,
    Unchanged,
    Removed,
    NotInstalled,
    /// Installed, but pointing at a binary that is gone or with a switch still off.
    Stale,
    /// The agent's config directory does not exist.
    NotFound,
    Refused,
}

impl Outcome {
    fn word(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Repaired => "repaired",
            Self::Unchanged => "unchanged",
            Self::Removed => "removed",
            Self::NotInstalled => "not installed",
            Self::Stale => "stale",
            Self::NotFound => "not found",
            Self::Refused => "refused",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AgentReport {
    pub agent: &'static str,
    pub outcome: Outcome,
    pub path: PathBuf,
    pub backup: Option<PathBuf>,
    pub notes: Vec<String>,
    pub diff: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub action: &'static str,
    pub dry_run: bool,
    pub agents: Vec<AgentReport>,
    /// Agents left out because no flag named them and their config directory does not exist.
    pub skipped: Vec<&'static str>,
}

pub async fn run(coord: Option<String>, args: HooksArgs, json: bool) -> Result<()> {
    let report = match args.cmd {
        HooksCmd::Summary => return home::run(coord, false, Mode::Hook).await,
        HooksCmd::Install(c) => agents::install(&c.which, c.dry_run)?,
        HooksCmd::Uninstall(c) => agents::uninstall(&c.which, c.dry_run)?,
        HooksCmd::Status(which) => status::run(&which)?,
    };
    super::out(json, &report, text);
    if report.agents.iter().any(|a| a.outcome == Outcome::Refused) {
        std::process::exit(1);
    }
    Ok(())
}

fn text(r: &Report) -> String {
    let home = dirs::home_dir();
    let mut out = Vec::new();
    if r.dry_run {
        out.push("dry run, nothing written".to_string());
    }
    for a in &r.agents {
        let path = collapse_home(&a.path, home.as_deref());
        out.push(format!("{:<9} {:<13} {path}", a.agent, a.outcome.word()));
        if let Some(b) = &a.backup {
            out.push(format!(
                "          backup: {}",
                collapse_home(b, home.as_deref())
            ));
        }
        out.extend(a.notes.iter().map(|n| format!("          {n}")));
        if r.dry_run && !a.diff.is_empty() {
            out.push(a.diff.trim_end().to_string());
        }
    }
    if !r.skipped.is_empty() {
        out.push(format!(
            "skipped: {} (not found; name one with --{} to set it up anyway)",
            r.skipped.join(", "),
            r.skipped[0]
        ));
    }
    if let Some(step) = next_step(r) {
        out.push(format!("help[1]: {step}"));
    }
    out.join("\n")
}

fn next_step(r: &Report) -> Option<&'static str> {
    let any = |o: Outcome| r.agents.iter().any(|a| a.outcome == o);
    match r.action {
        "status" if any(Outcome::Stale) || any(Outcome::NotInstalled) => {
            Some("Run `idlemax hooks install` to add or repair the hook")
        }
        "install" if !r.dry_run && (any(Outcome::Installed) || any(Outcome::Repaired)) => {
            Some("Run `idlemax hooks summary` to see what a new session will load")
        }
        _ => None,
    }
}
