//! Delivery: `idlemax plan`, which workers the dispatcher would launch now and why. It
//! launches nothing.

use anyhow::Result;
use clap::Args;

use super::out::dispatch as views;
use super::{ApiClient, Out};

#[derive(Args, Debug)]
pub struct PlanArgs {}

pub async fn run(_args: PlanArgs, c: &ApiClient, out: &Out) -> Result<()> {
    let plan = c.plan().await?;
    out.emit_with(
        &plan,
        views::plan_text,
        || Ok(views::plan_node(&plan)?),
        &views::plan_hints(&plan),
    )
}
