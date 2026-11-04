//! Delivery: process entry point; parses the CLI and dispatches.
#![forbid(unsafe_code)]

mod advisor;
mod agent;
mod cli;
mod coord;
mod state;
mod tui;

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("idlemax: {e:#}");
            ExitCode::FAILURE
        }
    }
}
