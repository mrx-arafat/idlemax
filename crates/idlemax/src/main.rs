//! Delivery: process entry point; parses the CLI and dispatches.
#![forbid(unsafe_code)]

mod advisor;
mod agent;
mod cli;
mod coord;
mod dispatch;
mod doctor;
mod error_text;
mod files;
mod live_config;
mod state;
mod tui;

use std::process::ExitCode;

fn main() -> ExitCode {
    // The job supervisor starts once per job and counts in the job's memory: it must not
    // start the async runtime or the CLI parser. It has no entry in `--help` for that reason.
    #[cfg(unix)]
    {
        let raw: Vec<_> = std::env::args_os().skip(1).collect();
        if raw
            .first()
            .is_some_and(|a| a == agent::supervise::SUBCOMMAND)
        {
            return ExitCode::from(agent::supervise::run(&raw[1..]));
        }
    }
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
