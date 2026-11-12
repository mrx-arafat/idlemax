//! Adapter: the JSON of the `herdr` subcommands the worker host calls, in the shapes seen on
//! Herdr 0.9.4 (`{"id": ..., "result": {...}}`). Only named fields are read and unknown keys
//! are ignored; a missing `result` is None.

use serde::de::DeserializeOwned;
use serde::Deserialize;

#[derive(Deserialize)]
struct Envelope<T> {
    result: T,
}

fn result<T: DeserializeOwned>(json: &str) -> Option<T> {
    serde_json::from_str::<Envelope<T>>(json)
        .ok()
        .map(|e| e.result)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Workspace {
    #[serde(alias = "workspace_id")]
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tab {
    #[serde(alias = "tab_id")]
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Pane {
    #[serde(alias = "pane_id")]
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub tab_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub pane: String,
    pub status: Option<String>,
    pub session: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub shell_pid: Option<u32>,
    pub foreground: Vec<u32>,
}

pub fn workspaces(json: &str) -> Option<Vec<Workspace>> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        workspaces: Vec<Workspace>,
    }
    result::<R>(json).map(|r| r.workspaces)
}

pub fn tabs(json: &str) -> Option<Vec<Tab>> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        tabs: Vec<Tab>,
    }
    result::<R>(json).map(|r| r.tabs)
}

pub fn panes(json: &str) -> Option<Vec<Pane>> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        panes: Vec<Pane>,
    }
    result::<R>(json).map(|r| r.panes)
}

pub fn agents(json: &str) -> Option<Vec<AgentRow>> {
    #[derive(Deserialize)]
    struct Session {
        value: Option<String>,
    }
    #[derive(Deserialize)]
    struct Row {
        pane_id: Option<String>,
        agent_status: Option<String>,
        agent_session: Option<Session>,
    }
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        agents: Vec<Row>,
    }
    let rows = result::<R>(json)?.agents.into_iter().filter_map(|r| {
        Some(AgentRow {
            pane: r.pane_id?,
            status: r.agent_status,
            session: r.agent_session.and_then(|s| s.value),
        })
    });
    Some(rows.collect())
}

pub fn process_info(json: &str) -> Option<ProcInfo> {
    #[derive(Deserialize)]
    struct Proc {
        pid: u32,
    }
    #[derive(Deserialize)]
    struct Info {
        shell_pid: Option<u32>,
        #[serde(default)]
        foreground_processes: Vec<Proc>,
    }
    #[derive(Deserialize)]
    struct R {
        process_info: Info,
    }
    let info = result::<R>(json)?.process_info;
    Some(ProcInfo {
        shell_pid: info.shell_pid,
        foreground: info.foreground_processes.iter().map(|p| p.pid).collect(),
    })
}

/// The text of `herdr agent read`: `result.text`, or `result.lines` joined.
// SHORTCUT: the field name of `agent read` is not yet observed; docs/WORKER-PROOF.md proves it.
pub fn read_text(json: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct R {
        text: Option<String>,
        lines: Option<Vec<String>>,
    }
    let r = result::<R>(json)?;
    r.text.or_else(|| r.lines.map(|l| l.join("\n")))
}
