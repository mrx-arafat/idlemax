//! Delivery: clap command line. Each subcommand makes one API call and prints the result.

mod client;
mod commands;
mod config_cmd;
mod dispatch;
pub(crate) use crate::files;
mod handlers;
mod home;
mod hooks;
mod inbox;
mod init;
mod init_cmd;
mod job;
mod job_wait;
mod logs;
mod mcp;
mod mode;
mod noop;
mod out;
mod plan;
pub mod print;
mod project;
pub mod report;
mod review;
pub mod rows;
mod schedule;
mod service;
mod setup;
mod skill;
mod statusline;
pub mod why;
mod workers;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use commands::Cmd;
use dispatch::{execute, hint};

pub use client::{ApiClient, ClientError, Proposal};
pub use commands::{AdviseCmd, LocalCmd};
pub use job::ExitWith;
pub use out::{CliError, Format, Out, Peer};

/// The bare version, what `-V`, `-v` and `--version` print.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
#[derive(Parser, Debug)]
#[command(
    name = "idlemax",
    about = "Run useful jobs on spare device capacity",
    disable_version_flag = true
)]
struct Cli {
    /// Coordinator URL [default: coordinator.url in config.toml]
    #[arg(long, global = true, env = "IDLEMAX_COORD", value_name = "URL")]
    coord: Option<String>,
    /// Output format [default: table on a terminal, toon when piped]
    #[arg(long, global = true, value_enum, value_name = "FORMAT")]
    format: Option<Format>,
    /// Same as --format json
    #[arg(long, global = true, conflicts_with = "format")]
    json: bool,
    /// Show event times in UTC instead of this machine's local time
    #[arg(long, global = true)]
    utc: bool,
    /// Do not cut long text in toon output
    #[arg(long, global = true)]
    full: bool,
    /// Add list columns to toon output, e.g. --fields cost,age (or all)
    #[arg(long, global = true, value_delimiter = ',', value_name = "FIELDS")]
    fields: Vec<String>,
    /// Print the version
    #[arg(short = 'V', short_alias = 'v', long, global = true)]
    version: bool,
    /// Without a command, print the live home view
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// Parses the command line, runs the command and returns the process exit code:
/// 0 success (no-ops included), 1 runtime error, 2 usage error.
pub async fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => return usage_exit(e, &args),
    };
    if cli.version {
        let _ = out::print_line(VERSION);
        return ExitCode::SUCCESS;
    }
    let format = Format::resolve(cli.format, cli.json, std::io::stdout().is_terminal());
    let out = Out::new(format, cli.full, cli.fields.clone());
    match execute(cli, &out).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast_ref::<ExitWith>() {
            // The command already printed its result; the code is the message.
            Some(ExitWith(code)) => ExitCode::from(*code),
            None => out.fail(&e),
        },
    }
}

/// A parse failure. Help and version output, and a person at a terminal, get clap's own
/// message; a pipe gets `error:` and the valid flags on stdout.
fn usage_exit(e: clap::Error, args: &[String]) -> ExitCode {
    let format = out::sniff_format(args, std::io::stdout().is_terminal());
    if !e.use_stderr() || format == Format::Table {
        e.exit();
    }
    let err = out::usage_error(&e, &mut Cli::command(), args);
    Out::new(format, false, Vec::new()).report(&err)
}

/// True when the whole command line is one version flag, so it can skip everything else.
pub fn is_version_probe(args: &[String]) -> bool {
    matches!(args, [a] if matches!(a.as_str(), "-V" | "-v" | "--version"))
}
