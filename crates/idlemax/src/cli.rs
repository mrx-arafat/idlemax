//! Delivery: clap command line. Each subcommand makes one API call and prints the result.

mod client;
mod files;
mod handlers;
mod home;
mod hooks;
mod inbox;
mod init;
mod job;
mod job_wait;
mod logs;
mod noop;
mod out;
pub mod print;
pub mod rows;
mod schedule;
mod service;
mod setup;
mod skill;
mod statusline;
pub mod why;

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use idlemax_core::protocol::ControlRequest;
use idlemax_core::types::{ControlAction, ControlScope, DeviceProfile, JobId, JobStatus};
use idlemax_core::Paths;
use serde::Serialize;

use crate::state::{self, now_ms};

pub use client::{ApiClient, ClientError};
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

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the device agent (one per device)
    Agent,
    /// Run the coordinator
    Coord,
    /// Create config.toml and the API token in $IDLEMAX_HOME (never overwrites)
    Init(InitArgs),
    /// Cluster summary and devices
    Status,
    /// List jobs
    Jobs {
        /// queued, leased, running, paused, succeeded, failed, killed, cancelled, parked
        #[arg(long)]
        status: Option<JobStatus>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Add or cancel a job
    #[command(subcommand)]
    Job(JobCmd),
    /// Repeat a job on an interval, such as every night
    #[command(subcommand)]
    Schedule(schedule::ScheduleCmd),
    /// Explain why a job is not running, and what to change
    Why { id: JobId },
    /// Pause jobs everywhere, or on one device, account or job
    Pause(Target),
    /// Resume paused jobs
    Resume(Target),
    /// Kill running jobs and hold new ones (kill switch)
    StopAll(Target),
    /// AI account windows, reserve and headroom
    Quota,
    /// Recent events
    Events {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Print the job log from this device
    Logs {
        /// Job ID
        id: JobId,
        /// Number of lines to show from the end of the log
        #[arg(long)]
        tail: Option<usize>,
        /// Follow new log output
        #[arg(short, long)]
        follow: bool,
    },
    /// List or show job result notes from the inbox (default: list)
    Inbox {
        #[command(subcommand)]
        cmd: Option<InboxCmd>,
    },
    /// AI advisor recommendations (default: list)
    Advise {
        #[command(subcommand)]
        cmd: Option<AdviseCmd>,
    },
    /// Set this device up in one command: init, the background service, then doctor (safe to repeat)
    Setup(setup::SetupArgs),
    /// Install or remove the background service (LaunchAgent or systemd user unit)
    Service(service::ServiceArgs),
    /// Wire the Claude Code statusline into the quota probe
    Statusline(statusline::StatuslineArgs),
    /// Interactive dashboard
    Tui,
    /// Check this device's setup; prints one line per check and the fix for anything wrong
    Doctor,
    /// Talk to this device's agent directly; works while the coordinator is down
    #[command(subcommand)]
    Local(LocalCmd),
    /// Session hooks for coding agents
    Hooks(hooks::HooksArgs),
    /// Print the agent skill, or check the committed copy is current
    Skill(skill::SkillArgs),
}

#[derive(Args, Debug)]
struct InitArgs {
    /// Guard preset [default: from the OS]
    #[arg(long, value_enum)]
    profile: Option<ProfileArg>,
    /// Coordinator URL agents and the CLI use, e.g. http://100.64.0.1:7878
    #[arg(long, value_name = "URL")]
    coord_url: Option<String>,
    /// Read the shared coordinator token from stdin instead of generating one
    #[arg(long)]
    token_stdin: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum ProfileArg {
    MacosLaptop,
    LinuxServer,
}

impl From<ProfileArg> for DeviceProfile {
    fn from(p: ProfileArg) -> Self {
        match p {
            ProfileArg::MacosLaptop => DeviceProfile::MacosLaptop,
            ProfileArg::LinuxServer => DeviceProfile::LinuxServer,
        }
    }
}

#[derive(Subcommand, Debug)]
enum JobCmd {
    /// Queue a job from a spec file, stdin (`-`) or a command after `--`
    Add(Box<job::AddArgs>),
    /// Cancel a job (kills it if running)
    Cancel { id: JobId },
}

#[derive(Subcommand, Debug)]
pub enum InboxCmd {
    /// List notes in the inbox (default)
    List,
    /// Print the note markdown for a job
    Show {
        /// Job ID
        id: JobId,
    },
}

#[derive(Subcommand, Debug)]
pub enum AdviseCmd {
    /// List recommendations, open ones first
    List,
    /// Queue the recommendation's suggested job
    Accept { id: i64 },
    /// Close the recommendation without acting
    Dismiss { id: i64 },
    /// Re-run the advisor now
    Refresh,
}

#[derive(Subcommand, Debug)]
pub enum LocalCmd {
    /// Agent state on this device
    Status,
    /// Pause every job on this device
    Pause,
    /// Resume after a local pause
    Resume,
    /// Kill every job on this device and hold new ones
    StopAll,
}

/// Control target; no flag means the whole cluster.
#[derive(Args, Debug)]
struct Target {
    #[arg(long, conflicts_with_all = ["account", "job"])]
    device: Option<String>,
    #[arg(long, conflicts_with = "job")]
    account: Option<String>,
    #[arg(long, value_name = "ID")]
    job: Option<JobId>,
    /// Recorded in the event log
    #[arg(long)]
    reason: Option<String>,
}

impl Target {
    fn request(self, action: ControlAction) -> ControlRequest {
        let scope = match (self.device, self.account, self.job) {
            (Some(d), _, _) => ControlScope::Device(d),
            (_, Some(a), _) => ControlScope::Account(a),
            (_, _, Some(j)) => ControlScope::Job(j),
            _ => ControlScope::Global,
        };
        ControlRequest {
            scope,
            action,
            reason: self.reason,
        }
    }
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

async fn execute(cli: Cli, out: &Out) -> Result<()> {
    let Cli {
        coord, cmd, utc, ..
    } = cli;
    let zone = if utc {
        rows::Zone::Utc
    } else {
        rows::Zone::Local
    };
    let Some(cmd) = cmd else {
        return home::run(coord, out, home::Mode::Full).await;
    };
    // Doctor and the hooks report a broken state dir or config instead of failing on it.
    let cmd = match cmd {
        Cmd::Doctor => return run_doctor(coord, out).await,
        Cmd::Hooks(args) => return hooks::run(coord, args, out).await,
        Cmd::Skill(args) => return skill::run(args, out),
        other => other,
    };
    let paths = state::paths_from_env()?;
    let cmd = match cmd {
        Cmd::Init(args) => return run_init(&paths, args, out),
        Cmd::Setup(args) => return setup::run(&paths, args, coord, out).await,
        Cmd::Service(args) => return service::run(&paths, args, out),
        Cmd::Statusline(args) => return statusline::run(&paths, args, out),
        other => other,
    };
    let config = state::load_config(&paths)?;
    match cmd {
        Cmd::Agent => {
            init_tracing();
            Ok(crate::agent::run(paths, config).await?)
        }
        Cmd::Coord => {
            init_tracing();
            Ok(crate::coord::run(paths, config).await?)
        }
        Cmd::Local(cmd) => {
            let client =
                ApiClient::new(&format!("http://{}", config.agent.listen), token(&paths)?)?;
            handlers::local(cmd, &client, out)
                .await
                .map_err(|e| hint(e, Peer::Agent))
        }
        Cmd::Inbox { cmd } => match cmd.unwrap_or(InboxCmd::List) {
            InboxCmd::List => inbox::run_list(&paths, out),
            InboxCmd::Show { id } => inbox::run_show(&paths, id, out),
        },
        Cmd::Job(JobCmd::Add(args)) => {
            let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
            let client = ApiClient::new(&url, token(&paths)?)?;
            job::add(&client, &paths, *args, out).await
        }
        Cmd::Logs { id, tail, follow } => {
            let log_path = paths.job_log_file(id);
            if log_path.exists() {
                logs::run_logs_local(&log_path, tail, follow).await
            } else {
                let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
                let client = ApiClient::new(&url, token(&paths)?)?;
                logs::run_logs_remote(&client, id, out).await
            }
        }
        cmd => {
            let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
            let client = ApiClient::new(&url, token(&paths)?)?;
            run_coord(cmd, &client, out, zone).await
        }
    }
}

/// Prints the report, then exits 1 when a check failed so scripts and CI see it.
async fn run_doctor(coord: Option<String>, out: &Out) -> Result<()> {
    let report = crate::doctor::run(coord).await;
    out.emit(&report, print::doctor, &[])?;
    if !report.healthy {
        return Err(ExitWith(1).into());
    }
    Ok(())
}

fn run_init(paths: &Paths, args: InitArgs, out: &Out) -> Result<()> {
    let token = if args.token_stdin {
        Some(read_token_stdin()?)
    } else {
        None
    };
    let opts = init::InitOptions {
        profile: args.profile.map(Into::into),
        coord_url: args.coord_url,
        token,
    };
    let report = init::init(paths, &opts)?;
    let word = |o: init::Outcome| {
        if o == init::Outcome::Created {
            "created"
        } else {
            "kept"
        }
    };
    let written = Written {
        config: Entry::new(word(report.config), paths.config_file()),
        token: Entry::new(word(report.token), paths.token_file()),
    };
    out.emit(
        &written,
        |w| {
            format!(
                "{} {}\n{} {}",
                w.config.action,
                w.config.path.display(),
                w.token.action,
                w.token.path.display()
            )
        },
        &out::hints::init(),
    )
}

/// What `init` did to one file.
#[derive(Serialize)]
struct Entry {
    action: &'static str,
    path: std::path::PathBuf,
}

impl Entry {
    fn new(action: &'static str, path: std::path::PathBuf) -> Self {
        Self { action, path }
    }
}

#[derive(Serialize)]
struct Written {
    config: Entry,
    token: Entry,
}

fn read_token_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .context("reading token from stdin")?;
    let t = s.trim();
    if t.is_empty() {
        bail!("empty token on stdin");
    }
    Ok(t.to_string())
}

fn token(paths: &Paths) -> Result<String> {
    state::load_token(paths).map_err(|e| {
        CliError::runtime(
            format!("no API token ({e})"),
            "idlemax init (or set IDLEMAX_TOKEN)",
        )
        .into()
    })
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // An already-installed subscriber is fine.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Replaces "cannot connect" with what to start, since the server is not there at all.
fn hint(e: impl Into<anyhow::Error>, peer: Peer) -> anyhow::Error {
    let e: anyhow::Error = e.into();
    match e.downcast_ref::<ClientError>() {
        Some(ClientError::Unreachable { url, .. }) => out::unreachable(peer, url).into(),
        _ => e,
    }
}

async fn run_coord(cmd: Cmd, c: &ApiClient, out: &Out, zone: rows::Zone) -> Result<()> {
    dispatch(cmd, c, out, now_ms(), zone)
        .await
        .map_err(|e| hint(e, Peer::Coordinator))
}

async fn dispatch(cmd: Cmd, c: &ApiClient, out: &Out, now: i64, zone: rows::Zone) -> Result<()> {
    match cmd {
        Cmd::Status => handlers::status(c, out, now).await,
        Cmd::Jobs { status, limit } => handlers::jobs(c, out, now, status, limit).await,
        Cmd::Why { id } => out.emit(&c.why(id).await?, why::render, &[]),
        Cmd::Job(JobCmd::Cancel { id }) => handlers::cancel(c, id, out).await,
        Cmd::Schedule(cmd) => schedule::run(cmd, c, out, now).await,
        Cmd::Pause(t) => handlers::control(c, t.request(ControlAction::Pause), out).await,
        Cmd::Resume(t) => handlers::control(c, t.request(ControlAction::Resume), out).await,
        Cmd::StopAll(t) => handlers::control(c, t.request(ControlAction::StopAll), out).await,
        Cmd::Quota => handlers::quota(c, out, now).await,
        Cmd::Events { limit } => handlers::events(c, out, limit, zone).await,
        Cmd::Advise { cmd } => handlers::advise(c, cmd.unwrap_or(AdviseCmd::List), out, now).await,
        Cmd::Tui => Ok(crate::tui::run(c.clone(), zone).await?),
        Cmd::Job(JobCmd::Add(_))
        | Cmd::Agent
        | Cmd::Coord
        | Cmd::Init(_)
        | Cmd::Local(_)
        | Cmd::Setup(_)
        | Cmd::Service(_)
        | Cmd::Statusline(_)
        | Cmd::Logs { .. }
        | Cmd::Inbox { .. }
        | Cmd::Doctor
        | Cmd::Hooks(_)
        | Cmd::Skill(_) => {
            bail!("not a coordinator command")
        }
    }
}
