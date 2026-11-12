//! Delivery: `idlemax config`: list, get, set and unset the settings in config.toml, then ask
//! the running agent to reload. The edit is the pure editor in `idlemax_core::config`.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use idlemax_core::config::{
    find_setting, list_settings, set_setting, show_value, unset_setting, Config, Edit, EditError,
    ReloadReport, Setting,
};
use idlemax_core::types::Os;
use idlemax_core::Paths;
use serde::Serialize;
use serde_json::Value;

use super::dispatch::token;
use super::out::toon::Node;
use super::{files, hint, out, print, ApiClient, CliError, ClientError, Out, Peer};

#[derive(Args, Debug)]
pub(super) struct ConfigArgs {
    #[command(subcommand)]
    cmd: Option<ConfigCmd>,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// List the settings you changed, or every setting with --all (default)
    List {
        /// Every setting with its default, not only the ones config.toml sets
        #[arg(long)]
        all: bool,
    },
    /// Show one setting: its value, its default and where the value comes from
    Get {
        /// section.name, such as guard.require_ac_power
        key: String,
    },
    /// Change one setting, save it and apply it to the running agent
    Set {
        /// section.name, such as guard.require_ac_power
        key: String,
        /// The new value: true, 40, a word, or a comma list such as git,cargo
        value: String,
    },
    /// Remove a setting from config.toml so its default applies again
    Unset {
        /// section.name, such as guard.require_ac_power
        key: String,
    },
}

impl ConfigArgs {
    /// True for `set` and `unset`: they change what idlemax does, so only the owner runs them.
    pub(super) fn changes_settings(&self) -> bool {
        matches!(
            self.cmd,
            Some(ConfigCmd::Set { .. } | ConfigCmd::Unset { .. })
        )
    }
}

pub(super) async fn run(paths: &Paths, args: ConfigArgs, out: &Out) -> Result<()> {
    match args.cmd.unwrap_or(ConfigCmd::List { all: false }) {
        ConfigCmd::List { all } => list(paths, all, out),
        ConfigCmd::Get { key } => get(paths, &key, out),
        ConfigCmd::Set { key, value } => change(paths, &key, Some(&value), out).await,
        ConfigCmd::Unset { key } => change(paths, &key, None, out).await,
    }
}

fn list(paths: &Paths, all: bool, out: &Out) -> Result<()> {
    let text = files::read_optional(&paths.config_file())?.unwrap_or_default();
    let settings: Vec<Setting> = list_settings(&text, Os::current())
        .map_err(|e| bad_file(paths, &e.to_string()))?
        .into_iter()
        .filter(|s| all || s.from_file)
        .collect();
    out.emit_with(
        &settings,
        |s| settings_table(s),
        || list_node(&settings),
        &out::hints::config_list(all),
    )
}

fn get(paths: &Paths, key: &str, out: &Out) -> Result<()> {
    let text = files::read_optional(&paths.config_file())?.unwrap_or_default();
    let setting = find_setting(&text, key, Os::current()).map_err(refused)?;
    out.emit_with(
        &setting,
        |s| {
            format!(
                "{} = {} ({})",
                s.key,
                show_value(&s.value),
                describe_source(s)
            )
        },
        || {
            Ok::<_, anyhow::Error>(Node::obj([
                ("setting", Node::str(&setting.key)),
                ("value", Node::of(&setting.value)?),
                ("default", Node::of(&setting.default)?),
                ("source", Node::str(source(&setting))),
            ]))
        },
        &[],
    )
}

async fn change(paths: &Paths, key: &str, value: Option<&str>, out: &Out) -> Result<()> {
    let file = paths.config_file();
    let Some(before) = files::read_optional(&file)? else {
        return Err(CliError::runtime(
            format!("{} does not exist yet", file.display()),
            "idlemax init",
        )
        .into());
    };
    let os = Os::current();
    let edit = match value {
        Some(v) => set_setting(&before, key, v, os),
        None => unset_setting(&before, key, os),
    }
    .map_err(refused)?;
    let rewrote = edit.text != before;
    let backup = if rewrote {
        files::save(&file, Some(&before), &edit.text)?
    } else {
        None
    };
    let agent = if edit.changed {
        ask_agent(paths, &before).await?
    } else {
        Agent::Unchanged
    };
    let outcome = Outcome::new(key, &edit, value.is_none(), rewrote, agent, backup);
    let hints = if edit.changed {
        out::hints::config_changed(key)
    } else {
        Vec::new()
    };
    out.emit_with(&outcome, Outcome::text, || Ok(outcome.node()), &hints)
}

/// What the running agent did with the new file.
enum Agent {
    /// The value did not change, so nobody was asked.
    Unchanged,
    NotRunning,
    /// An agent from before live reload: it has no such route.
    TooOld,
    Reloaded(ReloadReport),
}

/// Asks the agent at the address the file gave before this edit, which is where the running
/// one listens even when the edit moves it.
async fn ask_agent(paths: &Paths, running: &str) -> Result<Agent> {
    let config = Config::parse(running)?;
    let client = ApiClient::new(&format!("http://{}", config.agent.listen), token(paths)?)?;
    match client.local_reload().await {
        Ok(report) => Ok(Agent::Reloaded(report)),
        Err(ClientError::Unreachable { .. }) => Ok(Agent::NotRunning),
        Err(ClientError::Api { status: 404, .. }) => Ok(Agent::TooOld),
        Err(ClientError::Api {
            status: 400,
            message,
            ..
        }) => Err(CliError::runtime(
            format!("saved, but the agent refused the file and keeps its old settings: {message}"),
            "idlemax doctor",
        )
        .into()),
        Err(e) => Err(hint(e, Peer::Agent)),
    }
}

/// What `set` and `unset` report: the change, and what the agent did about it.
#[derive(Serialize)]
struct Outcome {
    key: String,
    before: Value,
    after: Value,
    changed: bool,
    agent: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<ReloadReport>,
    /// Where the file was copied before the edit, so the change can be undone by hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    backup: Option<PathBuf>,
    #[serde(skip)]
    headline: String,
    #[serde(skip)]
    note: Option<String>,
}

impl Outcome {
    fn new(
        key: &str,
        edit: &Edit,
        unset: bool,
        rewrote: bool,
        agent: Agent,
        backup: Option<PathBuf>,
    ) -> Self {
        let (state, report) = match &agent {
            Agent::Unchanged => ("unchanged", None),
            Agent::NotRunning => ("not_running", None),
            Agent::TooOld => ("too_old", None),
            Agent::Reloaded(report) => ("reloaded", Some(report.clone())),
        };
        Self {
            key: key.to_string(),
            before: edit.before.clone(),
            after: edit.after.clone(),
            changed: edit.changed,
            agent: state,
            headline: headline(key, edit, unset, rewrote),
            note: agent_note(key, &agent),
            report,
            backup,
        }
    }

    fn text(&self) -> String {
        let mut lines = vec![self.headline.clone()];
        lines.extend(self.note.iter().map(|n| format!("agent: {n}")));
        lines.extend(
            self.backup
                .iter()
                .map(|b| format!("backup: {}", b.display())),
        );
        lines.join("\n")
    }

    fn node(&self) -> Node {
        let mut fields = vec![("result", Node::str(&self.headline))];
        if let Some(note) = &self.note {
            fields.push(("agent", Node::str(note)));
        }
        if let Some(backup) = &self.backup {
            fields.push(("backup", Node::str(backup.display().to_string())));
        }
        Node::obj(fields)
    }
}

fn headline(key: &str, edit: &Edit, unset: bool, rewrote: bool) -> String {
    let (before, after) = (show_value(&edit.before), show_value(&edit.after));
    match (unset, edit.changed, rewrote) {
        (false, true, _) => format!("{key}: {before} -> {after}"),
        (false, false, _) => format!("{key} is already {after} (no-op)"),
        (true, true, _) => format!("{key}: {before} -> {after} (default restored)"),
        (true, false, true) => {
            format!("{key} removed from config.toml (the value stays {after}, the default)")
        }
        (true, false, false) => format!("{key} is already at its default {after} (no-op)"),
    }
}

/// One line on what the running agent did, if anyone was asked.
fn agent_note(key: &str, agent: &Agent) -> Option<String> {
    match agent {
        Agent::Unchanged => None,
        Agent::NotRunning => Some("not running; it uses the new value when it starts".to_string()),
        Agent::TooOld => Some(
            "saved; the running agent is older than this idlemax and cannot reload live, restart it to apply"
                .to_string(),
        ),
        Agent::Reloaded(report) => Some(if report.applies(key) {
            "applied now, no restart needed".to_string()
        } else if report.restart_agent.iter().any(|k| k == key) {
            "saved; restart the agent to apply it, it reads this only at start".to_string()
        } else if report.restart_coordinator.iter().any(|k| k == key) {
            "saved; restart the coordinator to apply it, it reads this only at start".to_string()
        } else {
            "saved".to_string()
        }),
    }
}

fn settings_table(settings: &[Setting]) -> String {
    if settings.is_empty() {
        return "no setting changed: every one is at its default (idlemax config list --all shows them)"
            .to_string();
    }
    let rows: Vec<Vec<String>> = settings
        .iter()
        .map(|s| {
            vec![
                s.key.clone(),
                show_value(&s.value),
                show_value(&s.default),
                source(s).to_string(),
            ]
        })
        .collect();
    print::table(&["SETTING", "VALUE", "DEFAULT", "SOURCE"], &rows)
}

/// One row of the TOON list: a cell is a plain scalar, so a list is shown as text and a
/// disabled check as `off`.
#[derive(Serialize)]
struct Row {
    setting: String,
    value: Value,
    default: Value,
    source: &'static str,
}

fn cell(value: &Value) -> Value {
    match value {
        Value::Null => Value::String("off".into()),
        Value::Array(_) | Value::Object(_) => Value::String(show_value(value)),
        other => other.clone(),
    }
}

fn list_node(settings: &[Setting]) -> Result<Node> {
    if settings.is_empty() {
        return Ok(Node::obj([(
            "settings",
            Node::str("0 changed: every setting is at its default"),
        )]));
    }
    let rows: Vec<Row> = settings
        .iter()
        .map(|s| Row {
            setting: s.key.clone(),
            value: cell(&s.value),
            default: cell(&s.default),
            source: source(s),
        })
        .collect();
    Ok(Node::obj([
        (
            "count",
            Node::Int(i64::try_from(rows.len()).unwrap_or(i64::MAX)),
        ),
        ("settings", Node::of(&rows)?),
    ]))
}

fn source(setting: &Setting) -> &'static str {
    if setting.from_file {
        "file"
    } else {
        "default"
    }
}

fn describe_source(setting: &Setting) -> String {
    let default = show_value(&setting.default);
    if setting.from_file {
        format!("set in config.toml, default {default}")
    } else {
        "the default".to_string()
    }
}

/// A key or value the editor refused is a usage error; a config.toml that is already broken
/// is a runtime one.
fn refused(error: EditError) -> anyhow::Error {
    let help = match &error {
        EditError::Unknown { .. } => {
            "idlemax config list --all shows every setting and its default".to_string()
        }
        EditError::NotEditable(_) => {
            "edit that section in config.toml by hand, then run `idlemax doctor`".to_string()
        }
        EditError::Invalid { key, .. } => {
            format!("idlemax config get {key} shows its value and its default")
        }
        EditError::Current(_) | EditError::Syntax(_) => {
            return CliError::runtime(error.to_string(), "idlemax doctor").into();
        }
    };
    CliError::usage(error.to_string(), help).into()
}

fn bad_file(paths: &Paths, why: &str) -> anyhow::Error {
    CliError::runtime(
        format!("{} is not valid: {why}", paths.config_file().display()),
        "fix it by hand, or run `idlemax doctor`",
    )
    .into()
}

#[cfg(test)]
mod tests;
