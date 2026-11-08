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
use super::out::toon::Node;
use super::rows::snake;
use super::{ExitWith, Out};

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

pub async fn run(coord: Option<String>, args: HooksArgs, out: &Out) -> Result<()> {
    let report = match args.cmd {
        HooksCmd::Summary => return home::run(coord, out, Mode::Hook).await,
        HooksCmd::Install(c) => agents::install(&c.which, c.dry_run)?,
        HooksCmd::Uninstall(c) => agents::uninstall(&c.which, c.dry_run)?,
        HooksCmd::Status(which) => status::run(&which)?,
    };
    let hints: Vec<String> = next_step(&report).iter().map(|s| s.to_string()).collect();
    out.emit_with(&report, text, || Ok(view(&report)), &hints)?;
    if report.agents.iter().any(|a| a.outcome == Outcome::Refused) {
        return Err(ExitWith(1).into());
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

/// The TOON report: one row per agent, the notes and any dry-run diff under it.
fn view(r: &Report) -> Node {
    let home = dirs::home_dir();
    let shown = |p: &std::path::Path| Node::Str(collapse_home(p, home.as_deref()));
    let rows = r.agents.iter().map(|a| {
        Node::obj([
            ("agent", Node::str(a.agent)),
            ("outcome", Node::str(snake(&a.outcome))),
            ("path", shown(&a.path)),
            ("backed_up", Node::Bool(a.backup.is_some())),
        ])
    });
    let mut fields = vec![
        ("action".to_string(), Node::str(r.action)),
        ("dry_run".to_string(), Node::Bool(r.dry_run)),
        ("agents".to_string(), Node::Arr(rows.collect())),
    ];
    let notes: Vec<Node> = r
        .agents
        .iter()
        .flat_map(|a| {
            a.notes
                .iter()
                .map(move |n| Node::Str(format!("{}: {n}", a.agent)))
        })
        .collect();
    if !notes.is_empty() {
        fields.push(("notes".into(), Node::Arr(notes)));
    }
    if !r.skipped.is_empty() {
        let skipped = r.skipped.iter().map(|s| Node::str(*s)).collect();
        fields.push(("skipped".into(), Node::Arr(skipped)));
    }
    let diff: Vec<Node> = r
        .agents
        .iter()
        .filter(|_| r.dry_run)
        .flat_map(|a| a.diff.trim_end().lines())
        .map(|l| Node::obj([("line", Node::str(l))]))
        .collect();
    if !diff.is_empty() {
        fields.push(("diff".into(), Node::Arr(diff)));
    }
    Node::Obj(fields)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::out::toon::encode;

    fn agent(name: &'static str, outcome: Outcome, notes: &[&str], diff: &str) -> AgentReport {
        AgentReport {
            agent: name,
            outcome,
            path: PathBuf::from(format!("/etc/{name}.json")),
            backup: None,
            notes: notes.iter().map(|n| n.to_string()).collect(),
            diff: diff.into(),
        }
    }

    #[test]
    fn the_toon_report_is_one_row_per_agent_with_notes_and_a_dry_run_diff() {
        let report = Report {
            action: "install",
            dry_run: true,
            agents: vec![
                agent("claude", Outcome::Installed, &[], "+ hook\n"),
                agent("codex", Outcome::Stale, &["binary gone"], ""),
            ],
            skipped: vec!["opencode"],
        };
        assert_eq!(
            encode(&view(&report)),
            "action: install\ndry_run: true\n\
             agents[2]{agent,outcome,path,backed_up}:\n  \
             claude,installed,/etc/claude.json,false\n  \
             codex,stale,/etc/codex.json,false\n\
             notes[1]: \"codex: binary gone\"\nskipped[1]: opencode\n\
             diff[1]{line}:\n  + hook"
        );
    }
}
