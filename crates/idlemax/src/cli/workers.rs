//! Delivery: `idlemax stop` and `idlemax start`, the owner's switch for every worker (D11).
//! `stop` cuts every worker's turn with its state saved and launches nothing until `start`;
//! `start` lets the same sessions continue. Pause, resume, stop-all and the PAUSE file keep
//! their own meaning.

use anyhow::Result;
use clap::Args;
use idlemax_core::control::scope_label;
use idlemax_core::types::ControlScope;
use idlemax_core::worker::control::{StartReply, StopReply, StopRequest};

use super::{ApiClient, Out};

#[derive(Args, Debug)]
pub struct StopArgs {
    /// Why, for the event log
    #[arg(long)]
    reason: Option<String>,
}

pub async fn stop(args: StopArgs, c: &ApiClient, out: &Out) -> Result<()> {
    let req = StopRequest {
        scope: ControlScope::Global,
        reason: args.reason,
    };
    let reply = c.stop_workers(&req).await?;
    let hints = vec![hint("idlemax start", "let the same workers continue")];
    out.emit(&reply, stop_text, &hints)
}

pub async fn start(c: &ApiClient, out: &Out) -> Result<()> {
    let reply = c.start_workers().await?;
    let mut hints: Vec<String> = reply
        .paused
        .iter()
        .map(|scope| hint(&resume_command(scope), "lift a pause that still holds them"))
        .collect();
    hints.push(hint(
        "idlemax plan",
        "see what the dispatcher launches next",
    ));
    out.emit(&reply, start_text, &hints)
}

fn stop_text(reply: &StopReply) -> String {
    let what = match reply.workers.len() {
        0 => "nothing to do: no worker was running".to_string(),
        n => format!("stopped {n} workers with their state saved"),
    };
    format!("{what}; nothing launches until `idlemax start`")
}

fn start_text(reply: &StartReply) -> String {
    let mut lines = vec![match reply.workers.len() {
        0 => "workers may launch again; no worker was stopped".to_string(),
        n => format!("{n} workers go back in the queue and continue their sessions"),
    }];
    for scope in &reply.paused {
        lines.push(format!(
            "still paused: {}; run `{}` to lift it",
            scope_label(scope),
            resume_command(scope)
        ));
    }
    lines.join("\n")
}

fn resume_command(scope: &ControlScope) -> String {
    match scope {
        ControlScope::Global => "idlemax resume".into(),
        ControlScope::Device(d) => format!("idlemax resume --device {d}"),
        ControlScope::Account(a) => format!("idlemax resume --account {a}"),
        ControlScope::Job(id) => format!("idlemax resume --job {id}"),
    }
}

fn hint(cmd: &str, to: &str) -> String {
    format!("Run `{cmd}` to {to}")
}
