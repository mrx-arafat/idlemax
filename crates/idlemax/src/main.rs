//! Delivery: process entry point; parses the CLI and dispatches.
#![forbid(unsafe_code)]

mod advisor;
mod agent;
mod cli;
mod coord;
mod doctor;
mod state;
mod tui;

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast_ref::<cli::ExitWith>() {
            // The command already printed its result; the code is the message.
            Some(cli::ExitWith(code)) => ExitCode::from(*code),
            None => {
                eprintln!("idlemax: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}
