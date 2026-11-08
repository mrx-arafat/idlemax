//! Delivery: process entry point; parses the CLI and dispatches.
#![forbid(unsafe_code)]

mod advisor;
mod agent;
mod cli;
mod coord;
mod doctor;
mod error_text;
mod state;
mod tui;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if cli::is_version_probe(&args) {
        println!("{}", cli::VERSION);
        return ExitCode::SUCCESS;
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(cli::run()),
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            ExitCode::FAILURE
        }
    }
}
