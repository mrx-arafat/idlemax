//! Delivery: `idlemax hooks`. `summary` prints the compact view a session hook loads.

use anyhow::Result;
use clap::{Args, Subcommand};

use super::home::{self, Mode};

#[derive(Args, Debug)]
pub struct HooksArgs {
    #[command(subcommand)]
    cmd: HooksCmd,
}

#[derive(Subcommand, Debug)]
enum HooksCmd {
    /// Print the compact summary a session hook loads (always exits 0)
    Summary,
}

pub async fn run(coord: Option<String>, args: HooksArgs, _json: bool) -> Result<()> {
    match args.cmd {
        HooksCmd::Summary => home::run(coord, false, Mode::Hook).await,
    }
}
