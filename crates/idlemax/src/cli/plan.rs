//! Delivery: `idlemax plan`, which workers the dispatcher would launch now and why. It
//! launches nothing; `--refresh` asks for an orchestrator pass now.

use anyhow::Result;
use clap::Args;

use super::out::dispatch as views;
use super::{ApiClient, Out};

#[derive(Args, Debug)]
pub struct PlanArgs {
    /// Ask for an orchestrator pass now (one `claude -p --model opus` job; it spends quota)
    #[arg(long)]
    refresh: bool,
}

pub async fn run(args: PlanArgs, c: &ApiClient, out: &Out) -> Result<()> {
    let plan = if args.refresh {
        c.refresh_plan().await?
    } else {
        c.plan().await?
    };
    let mut hints = views::plan_hints(&plan);
    if args.refresh {
        hints.push(views::refresh_hint());
    }
    out.emit_with(
        &plan,
        views::plan_text,
        || Ok(views::plan_node(&plan)?),
        &hints,
    )
}
