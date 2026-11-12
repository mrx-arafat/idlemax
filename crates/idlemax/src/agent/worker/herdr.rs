//! Adapter: the Herdr worker host. Workers live in one workspace labelled `idlemax workers`,
//! one tab per project and one pane per worker (D15). Every `herdr` call goes through
//! `Commands` with a 10 s limit, names its own workspace or pane, never takes focus, and a
//! pane is closed only while its label and workspace still match what was saved (D4).

mod parse;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use idlemax_core::worker::report::MAX_END_TEXT_CHARS;
use tracing::warn;

use super::cmd::{Cmd, Commands};
use super::host::{AgentStatus, Handle, HostError, HostState, Launch, WorkerHost};
use parse::Pane;

/// The workspace that holds every worker pane.
pub const WORKSPACE_LABEL: &str = "idlemax workers";
const HERDR_LIMIT: Duration = Duration::from_secs(10);
/// Lines of the agent's recent output read for `end_text`.
const END_LINES: &str = "40";

pub struct HerdrHost {
    commands: Arc<dyn Commands>,
    /// The program that puts a pid into the background class (A16); None where there is none.
    background: Option<String>,
}

impl HerdrHost {
    pub fn new(commands: Arc<dyn Commands>) -> Self {
        let background = cfg!(target_os = "macos").then(|| "/usr/sbin/taskpolicy".to_string());
        Self::with_background(commands, background)
    }

    pub fn with_background(commands: Arc<dyn Commands>, background: Option<String>) -> Self {
        Self {
            commands,
            background,
        }
    }

    /// One `herdr` call: its stdout, or why it failed.
    fn call(&self, args: &[String]) -> Result<String, HostError> {
        run(self.commands.as_ref(), "herdr", args)
    }

    fn parsed<T>(
        &self,
        args: &[&str],
        what: &str,
        parse: fn(&str) -> Option<T>,
    ) -> Result<T, HostError> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&self.call(&args)?).ok_or_else(|| HostError::Missing {
            call: label("herdr", &args),
            what: what.into(),
        })
    }

    fn panes(&self, workspace: Option<&str>) -> Result<Vec<Pane>, HostError> {
        let mut args = vec!["pane", "list"];
        args.extend(workspace.map(|w| ["--workspace", w]).into_iter().flatten());
        self.parsed(&args, "panes", parse::panes)
    }

    fn workspace(&self) -> Result<Option<String>, HostError> {
        let all = self.parsed(&["workspace", "list"], "workspaces", parse::workspaces)?;
        Ok(all
            .into_iter()
            .find(|w| w.label.as_deref() == Some(WORKSPACE_LABEL))
            .map(|w| w.id))
    }

    /// Makes the worker's pane; when that fails after something was made, the panes this
    /// call saw appear are closed again, so no empty workspace or unnamed pane stays behind.
    fn new_pane(&self, launch: &Launch) -> Result<(String, String), HostError> {
        let existing = self.workspace()?;
        let before = match &existing {
            Some(ws) => self.panes(Some(ws))?,
            None => Vec::new(),
        };
        let made = self.make_pane(launch, existing, &before);
        if made.is_err() {
            self.close_new(&before);
        }
        made
    }

    /// In a new workspace, a new tab of the project, or split from a pane of the project's
    /// tab. Returns the workspace and the new pane.
    fn make_pane(
        &self,
        launch: &Launch,
        existing: Option<String>,
        before: &[Pane],
    ) -> Result<(String, String), HostError> {
        let folder = launch.folder.display().to_string();
        let Some(ws) = existing else {
            let mut args = strings(&["workspace", "create", "--label", WORKSPACE_LABEL]);
            args.extend(strings(&["--cwd", &folder]));
            self.call(&with_env(args, &launch.env))?;
            let ws = self
                .workspace()?
                .ok_or_else(|| missing("workspace list", "workspace"))?;
            let tabs = self.parsed(&["tab", "list", "--workspace", &ws], "tabs", parse::tabs)?;
            let tab = tabs.first().ok_or_else(|| missing("tab list", "tab"))?;
            self.call(&strings(&["tab", "rename", &tab.id, &launch.tab]))?;
            let pane = only_new(&[], &self.panes(Some(&ws))?)?;
            return Ok((ws, pane));
        };
        let tabs = self.parsed(&["tab", "list", "--workspace", &ws], "tabs", parse::tabs)?;
        let tab = tabs
            .iter()
            .find(|t| t.label.as_deref() == Some(launch.tab.as_str()));
        let args = match tab {
            Some(tab) => {
                let from = before
                    .iter()
                    .find(|p| p.tab_id.as_deref() == Some(tab.id.as_str()))
                    .ok_or_else(|| missing("pane list", "pane of the project's tab"))?;
                let mut args = strings(&["pane", "split", "--pane", &from.id]);
                args.extend(strings(&["--direction", "down", "--cwd", &folder]));
                args
            }
            None => {
                let mut args = strings(&["tab", "create", "--workspace", &ws, "--cwd", &folder]);
                args.extend(strings(&["--label", &launch.tab]));
                args
            }
        };
        self.call(&with_env(args, &launch.env))?;
        let pane = only_new(before, &self.panes(Some(&ws))?)?;
        Ok((ws, pane))
    }

    /// Closes every pane of the worker workspace that `before` did not list.
    fn close_new(&self, before: &[Pane]) {
        let Ok(Some(ws)) = self.workspace() else {
            return;
        };
        let Ok(after) = self.panes(Some(&ws)) else {
            return;
        };
        for pane in after
            .iter()
            .filter(|p| !before.iter().any(|b| b.id == p.id))
        {
            let _ = self.call(&strings(&["pane", "close", &pane.id]));
        }
    }

    /// The worker's pane while it still carries the saved label and workspace; None when it
    /// is gone, `NotOurs` when the id now names another pane. Every call that touches the
    /// pane checks this first, so no prompt lands in, and no text is read from, a pane that
    /// is not the worker's (D4, A9).
    fn verified(&self, handle: &Handle) -> Result<Option<Pane>, HostError> {
        let panes = self.panes(None)?;
        let Some(pane) = panes.into_iter().find(|p| p.id == handle.pane) else {
            return Ok(None);
        };
        let ours = pane.label.as_deref() == Some(handle.label.as_str())
            && pane.workspace_id.as_deref() == Some(handle.workspace.as_str());
        match ours {
            true => Ok(Some(pane)),
            false => Err(HostError::NotOurs {
                pane: handle.pane.clone(),
            }),
        }
    }

    /// Names the pane, puts its shell in the background class and starts the agent in it.
    fn start_in(&self, pane: &str, launch: &Launch) -> Result<(), HostError> {
        self.call(&strings(&["pane", "rename", pane, &launch.name]))?;
        if let Some(program) = &self.background {
            let info = self.process_info(pane)?;
            let pid = info
                .shell_pid
                .ok_or_else(|| missing("pane process-info", "shell pid"))?;
            run(
                self.commands.as_ref(),
                program,
                &strings(&["-b", "-p", &pid.to_string()]),
            )?;
        }
        let mut args = strings(&["agent", "start", &launch.name]);
        args.extend(strings(&[
            "--kind",
            launch.cli.as_str(),
            "--pane",
            pane,
            "--",
        ]));
        args.extend(launch.args.iter().cloned());
        self.call(&args).map(drop)
    }

    fn process_info(&self, pane: &str) -> Result<parse::ProcInfo, HostError> {
        let args = ["pane", "process-info", "--pane", pane];
        self.parsed(&args, "process_info", parse::process_info)
    }
}

impl WorkerHost for HerdrHost {
    fn start(&self, launch: &Launch) -> Result<Handle, HostError> {
        let (workspace, pane) = self.new_pane(launch)?;
        let handle = Handle {
            pane,
            workspace,
            label: launch.name.clone(),
        };
        if let Err(e) = self.start_in(&handle.pane, launch) {
            // Not briefed and not ours to keep: the pane goes, and the start failed.
            let _ = self.call(&strings(&["pane", "close", &handle.pane]));
            return Err(e);
        }
        Ok(handle)
    }

    fn state(&self, handle: &Handle) -> Result<HostState, HostError> {
        let gone = HostState {
            status: AgentStatus::Gone,
            session: None,
            shell_pid: None,
            foreground: Vec::new(),
            exit_code: None,
        };
        match self.verified(handle) {
            Ok(Some(_)) => {}
            Ok(None) | Err(HostError::NotOurs { .. }) => return Ok(gone),
            Err(e) => return Err(e),
        }
        let agents = self.parsed(&["agent", "list"], "agents", parse::agents)?;
        let row = agents.into_iter().find(|a| a.pane == handle.pane);
        let info = self.process_info(&handle.pane)?;
        Ok(HostState {
            status: status_of(row.as_ref().and_then(|r| r.status.as_deref())),
            session: row.and_then(|r| r.session),
            shell_pid: info.shell_pid,
            foreground: info.foreground,
            exit_code: None,
        })
    }

    fn send(&self, handle: &Handle, text: &str) -> Result<(), HostError> {
        if self.verified(handle)?.is_none() {
            return Err(missing("pane list", "worker pane"));
        }
        self.call(&strings(&["agent", "prompt", &handle.pane, text]))
            .map(drop)
    }

    fn stop(&self, handle: &Handle) -> Result<(), HostError> {
        if self.verified(handle)?.is_none() {
            return Ok(());
        }
        self.call(&strings(&["pane", "close", &handle.pane]))
            .map(drop)
    }

    fn matches(&self, handle: &Handle) -> Result<bool, HostError> {
        match self.verified(handle) {
            Ok(pane) => Ok(pane.is_some()),
            Err(HostError::NotOurs { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn end_text(&self, handle: &Handle) -> Option<String> {
        self.verified(handle).ok().flatten()?;
        let args = strings(&["agent", "read", &handle.pane, "--source", "recent"]);
        let args = [args, strings(&["--lines", END_LINES])].concat();
        let Some(text) = self.call(&args).ok().and_then(|out| parse::read_text(&out)) else {
            // A limit message read as no text makes the Run look like a turn with no commit.
            warn!(pane = %handle.pane, "herdr agent read gave no text; end_text is unknown");
            return None;
        };
        let tail = last_lines(&text, 40);
        let skip = tail.chars().count().saturating_sub(MAX_END_TEXT_CHARS);
        Some(tail.chars().skip(skip).collect())
    }

    fn is_available(&self) -> bool {
        self.parsed(&["workspace", "list"], "workspaces", parse::workspaces)
            .is_ok()
    }
}

fn run(commands: &dyn Commands, program: &str, args: &[String]) -> Result<String, HostError> {
    let cmd = Cmd {
        program: program.into(),
        args: args.to_vec(),
        cwd: None,
        env: Vec::new(),
        stdin: None,
        limit: HERDR_LIMIT,
        throttle: false,
        tag: None,
    };
    let call = label(program, args);
    let out = commands.run(&cmd).map_err(|source| HostError::Cmd {
        call: call.clone(),
        source,
    })?;
    if out.code != Some(0) {
        let why: String = out.stderr.trim().chars().take(300).collect();
        return Err(HostError::Failed { call, why });
    }
    Ok(out.stdout)
}

/// The pane in `after` that `before` did not have; exactly one, or the start is unconfirmed.
fn only_new(before: &[Pane], after: &[Pane]) -> Result<String, HostError> {
    let mut new = after
        .iter()
        .filter(|p| !before.iter().any(|b| b.id == p.id));
    match (new.next(), new.next()) {
        (Some(pane), None) => Ok(pane.id.clone()),
        _ => Err(missing("pane list", "single new pane")),
    }
}

fn status_of(status: Option<&str>) -> AgentStatus {
    match status {
        Some("idle") => AgentStatus::Idle,
        Some("working") => AgentStatus::Working,
        Some("blocked") => AgentStatus::Blocked,
        Some("done") => AgentStatus::Done,
        _ => AgentStatus::Unknown,
    }
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn with_env(mut args: Vec<String>, env: &[(String, String)]) -> Vec<String> {
    for (k, v) in env {
        args.push("--env".into());
        args.push(format!("{k}={v}"));
    }
    args.push("--no-focus".into());
    args
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The program and the first two words of a call, for error messages (never the prompt).
fn label(program: &str, args: &[String]) -> String {
    let words: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
    format!("{program} {}", words.join(" "))
}

fn missing(call: &str, what: &str) -> HostError {
    HostError::Missing {
        call: format!("herdr {call}"),
        what: what.into(),
    }
}
