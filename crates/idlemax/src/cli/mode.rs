//! Delivery: `idlemax mode <off|propose|auto>`, the dispatcher's mode switch (A1).

use anyhow::Result;
use clap::{Args, ValueEnum};
use idlemax_core::dispatch::Mode;

use super::out::dispatch as views;
use super::{noop, ApiClient, Out};

#[derive(Args, Debug)]
pub struct ModeArgs {
    /// off plans nothing; propose shows what the dispatcher would launch and launches nothing;
    /// auto launches the planned workers one at a time
    mode: ModeArg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ModeArg {
    Off,
    Propose,
    Auto,
}

pub async fn run(args: ModeArgs, c: &ApiClient, out: &Out) -> Result<()> {
    let mode = match args.mode {
        ModeArg::Off => Mode::Off,
        ModeArg::Propose => Mode::Propose,
        ModeArg::Auto => Mode::Auto,
    };
    let reply = c.set_mode(mode).await?;
    let text = if reply.changed {
        format!("dispatcher mode: {}", reply.mode)
    } else {
        noop::already(reply.mode)
    };
    out.emit_with(
        &reply,
        |_| text.clone(),
        || Ok(views::mode_result(&text, &reply)),
        &views::mode_hints(reply.mode),
    )
}
