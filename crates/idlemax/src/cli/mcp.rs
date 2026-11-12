//! Delivery: `idlemax mcp`, an MCP server over stdio (D12) that is a thin client of the
//! coordinator API. The handshake and the tool list need no token; each tool loads the token
//! when called. `start` and `project_on` refuse first when this process was launched by
//! idlemax, and the coordinator checks again (Task 4).

use std::io::IsTerminal;

use anyhow::Result;
use idlemax_core::control::caller::{CallerError, LAUNCHED_ENV};
use idlemax_core::mcp::{handle, tool_result, Step, Tool};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::handlers::status_node;
use super::out::{toon, unreachable, OWNER_ONLY_HELP};
use super::{dispatch::token, ApiClient, CliError, ClientError, ExitWith, Peer};
use crate::state::{self, now_ms};

const HOW_TO_ADD: &str = "idlemax mcp is an MCP server for agent CLIs; it reads JSON-RPC on stdin.
Add it to an agent CLI with one of:
  claude mcp add --scope user idlemax -- idlemax mcp
  codex mcp add idlemax -- idlemax mcp
  opencode mcp add --global idlemax -- idlemax mcp
  agy mcp add idlemax idlemax mcp";

pub async fn run(coord: Option<String>) -> Result<()> {
    if std::io::stdin().is_terminal() {
        eprintln!("{HOW_TO_ADD}");
        return Err(ExitWith(2).into());
    }
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match handle(&line) {
            Step::Quiet => continue,
            Step::Reply(text) => text,
            Step::Call { id, tool } => match call(coord.clone(), tool).await {
                Ok(text) => tool_result(&id, &text, false),
                Err(why) => tool_result(&id, &why, true),
            },
        };
        stdout.write_all(reply.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// The tool's text, or what went wrong and the next step.
async fn call(coord: Option<String>, tool: Tool) -> Result<String, String> {
    if matches!(tool, Tool::Start | Tool::ProjectOn(_)) && std::env::var_os(LAUNCHED_ENV).is_some()
    {
        let why = CallerError::Launched(std::process::id()).to_string();
        return Err(format!("{why}\nnext: {OWNER_ONLY_HELP}"));
    }
    let c = client(coord)?;
    run_tool(&c, tool).await.map_err(explain)
}

fn client(coord: Option<String>) -> Result<ApiClient, String> {
    let setup = |e: anyhow::Error| format!("{e}\nnext: run `idlemax setup` on this device");
    let paths = state::paths_from_env().map_err(|e| setup(e.into()))?;
    let config = state::load_config(&paths).map_err(|e| setup(e.into()))?;
    let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
    ApiClient::new(&url, token(&paths).map_err(setup)?).map_err(|e| setup(e.into()))
}

async fn run_tool(c: &ApiClient, tool: Tool) -> Result<String> {
    Ok(match tool {
        Tool::Status => {
            let s = c.status().await?;
            let view = match c.dispatcher().await {
                Err(ClientError::Api { status: 404, .. }) => None,
                other => Some(other?),
            };
            toon::encode(&status_node(&s, view.as_ref(), now_ms(), &[])?)
        }
        Tool::Start => serde_json::to_string(&c.start_workers().await?)?,
        Tool::Stop => {
            serde_json::to_string(&c.stop_all_workers("stopped through idlemax mcp").await?)?
        }
        Tool::ProjectOn(name) => switch(c, &name, true).await?,
        Tool::ProjectOff(name) => switch(c, &name, false).await?,
        Tool::FinishedWork => serde_json::to_string_pretty(&c.dispatcher().await?.review)?,
    })
}

/// Switches the one project with that name; a name on two devices is refused.
async fn switch(c: &ApiClient, name: &str, on: bool) -> Result<String> {
    let all = c.projects().await?;
    let matching: Vec<_> = all.iter().filter(|v| v.project.name == name).collect();
    let view = match matching.as_slice() {
        [one] => c.switch_project(one.project.id, on).await?,
        [] => {
            let msg = format!("no project named {name}");
            return Err(CliError::runtime(msg, "idlemax project (lists them)").into());
        }
        many => {
            let devices: Vec<&str> = many.iter().map(|v| v.project.device.as_str()).collect();
            let msg = format!("{name} is a project on {}", devices.join(", "));
            let help = "switch it with `idlemax project on|off <name> --device <device>`";
            return Err(CliError::runtime(msg, help).into());
        }
    };
    let word = if view.project.enabled { "on" } else { "off" };
    Ok(format!(
        "project {} on {}: {word}",
        view.project.name, view.project.device
    ))
}

fn explain(e: anyhow::Error) -> String {
    let e = match e.downcast_ref::<ClientError>() {
        Some(ClientError::Unreachable { url, .. }) => unreachable(Peer::Coordinator, url).into(),
        _ => e,
    };
    match e.downcast_ref::<CliError>() {
        Some(CliError {
            what,
            help: Some(help),
            ..
        }) => format!("{what}\nnext: {help}"),
        _ => e.to_string(),
    }
}
