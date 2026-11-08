//! Delivery: clap command line. Each subcommand makes one API call and prints the result.

mod client;
mod files;
mod home;
mod hooks;
mod inbox;
mod init;
mod job;
mod job_wait;
mod logs;
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

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use idlemax_core::protocol::ControlRequest;
use idlemax_core::types::{ControlAction, ControlScope, DeviceProfile, JobId, JobStatus};
use idlemax_core::Paths;
use serde::Serialize;

use crate::state::{self, now_ms};

pub use client::{ApiClient, ClientError};
pub use job::ExitWith;
pub use out::{Format, Out};

#[derive(Parser, Debug)]
#[command(
    name = "idlemax",
    version,
    about = "Run useful jobs on spare device capacity"
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
enum AdviseCmd {
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
enum LocalCmd {
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

pub async fn run() -> Result<()> {
    let Cli {
        coord,
        format,
        json,
        full,
        utc,
        cmd,
    } = Cli::parse();
    let zone = if utc {
        rows::Zone::Utc
    } else {
        rows::Zone::Local
    };
    let out = Out::new(
        Format::resolve(format, json, std::io::stdout().is_terminal()),
        full,
    );
    let Some(cmd) = cmd else {
        return home::run(coord, &out, home::Mode::Full).await;
    };
    // Doctor and the hooks report a broken state dir or config instead of failing on it.
    let cmd = match cmd {
        Cmd::Doctor => return run_doctor(coord, &out).await,
        Cmd::Hooks(args) => return hooks::run(coord, args, &out).await,
        Cmd::Skill(args) => return skill::run(args, &out),
        other => other,
    };
    let paths = state::paths_from_env()?;
    let cmd = match cmd {
        Cmd::Init(args) => return run_init(&paths, args, &out),
        Cmd::Setup(args) => return setup::run(&paths, args, coord, &out).await,
        Cmd::Service(args) => return service::run(&paths, args, &out),
        Cmd::Statusline(args) => return statusline::run(&paths, args, &out),
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
            run_local(cmd, &client, &out)
                .await
                .map_err(|e| hint(e, LOCAL_DOWN))
        }
        Cmd::Inbox { cmd } => match cmd.unwrap_or(InboxCmd::List) {
            InboxCmd::List => inbox::run_list(&paths, &out),
            InboxCmd::Show { id } => inbox::run_show(&paths, id, &out),
        },
        Cmd::Job(JobCmd::Add(args)) => {
            let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
            let client = ApiClient::new(&url, token(&paths)?)?;
            job::add(&client, &paths, *args, &out).await
        }
        Cmd::Logs { id, tail, follow } => {
            let log_path = paths.job_log_file(id);
            if log_path.exists() {
                logs::run_logs_local(&log_path, tail, follow).await
            } else {
                let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
                let client = ApiClient::new(&url, token(&paths)?)?;
                logs::run_logs_remote(&client, &log_path, id).await
            }
        }
        cmd => {
            let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
            let client = ApiClient::new(&url, token(&paths)?)?;
            run_coord(cmd, &client, &out, zone).await
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
        &[],
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
    state::load_token(paths).context("no API token; run `idlemax init` or set IDLEMAX_TOKEN")
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

const COORD_DOWN: &str =
    "coordinator unreachable; `idlemax local stop-all` still works on this device";
const LOCAL_DOWN: &str =
    "local agent unreachable; is `idlemax agent` running? touch $IDLEMAX_HOME/PAUSE to pause it";

/// Adds a next step when the server is not there at all.
fn hint(e: impl Into<anyhow::Error>, msg: &'static str) -> anyhow::Error {
    let e: anyhow::Error = e.into();
    if matches!(
        e.downcast_ref::<ClientError>(),
        Some(ClientError::Unreachable { .. })
    ) {
        e.context(msg)
    } else {
        e
    }
}

async fn run_coord(cmd: Cmd, c: &ApiClient, out: &Out, zone: rows::Zone) -> Result<()> {
    dispatch(cmd, c, out, now_ms(), zone)
        .await
        .map_err(|e| hint(e, COORD_DOWN))
}

async fn dispatch(cmd: Cmd, c: &ApiClient, out: &Out, now: i64, zone: rows::Zone) -> Result<()> {
    match cmd {
        Cmd::Status => out.emit(&c.status().await?, |s| print::status(s, now), &[]),
        Cmd::Jobs { status, limit } => {
            out.emit(&c.jobs(status, limit).await?, |j| print::jobs(j, now), &[])
        }
        Cmd::Why { id } => out.emit(&c.why(id).await?, why::render, &[]),
        Cmd::Job(JobCmd::Cancel { id }) => out.emit(&c.cancel_job(id).await?, print::job_line, &[]),
        Cmd::Schedule(cmd) => schedule::run(cmd, c, out, now).await,
        Cmd::Pause(t) => control(c, t.request(ControlAction::Pause), out).await,
        Cmd::Resume(t) => control(c, t.request(ControlAction::Resume), out).await,
        Cmd::StopAll(t) => control(c, t.request(ControlAction::StopAll), out).await,
        Cmd::Quota => quota(c, out, now).await,
        Cmd::Events { limit } => out.emit(&c.events(limit).await?, |e| print::events(e, zone), &[]),
        Cmd::Advise { cmd } => advise(c, cmd.unwrap_or(AdviseCmd::List), out, now).await,
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

async fn control(c: &ApiClient, req: ControlRequest, out: &Out) -> Result<()> {
    let s = c.control(&req).await?;
    out.emit(
        &s,
        |s| format!("ok: {}\n{}", print::control_label(&req), print::summary(s)),
        &[],
    )
}

async fn quota(c: &ApiClient, out: &Out, now: i64) -> Result<()> {
    let (views, status) = tokio::try_join!(c.quota(), c.status())?;
    out.emit(
        &views,
        |v| print::quota(v, &status.paused_accounts, now),
        &[],
    )
}

async fn advise(c: &ApiClient, cmd: AdviseCmd, out: &Out, now: i64) -> Result<()> {
    match cmd {
        AdviseCmd::List => out.emit(&c.advice().await?, |a| print::advice(a, now), &[]),
        AdviseCmd::Refresh => out.emit(&c.refresh_advice().await?, |a| print::advice(a, now), &[]),
        AdviseCmd::Accept { id } => out.emit(&c.accept_advice(id).await?, print::job_line, &[]),
        AdviseCmd::Dismiss { id } => {
            out.emit(&c.dismiss_advice(id).await?, print::advice_line, &[])
        }
    }
}

async fn run_local(cmd: LocalCmd, c: &ApiClient, out: &Out) -> Result<()> {
    let action = match cmd {
        LocalCmd::Status => None,
        LocalCmd::Pause => Some(ControlAction::Pause),
        LocalCmd::Resume => Some(ControlAction::Resume),
        LocalCmd::StopAll => Some(ControlAction::StopAll),
    };
    let s = match action {
        None => c.local_status().await?,
        Some(action) => {
            let req = ControlRequest {
                scope: ControlScope::Global,
                action,
                reason: None,
            };
            c.local_control(&req).await?
        }
    };
    out.emit(&s, |s| print::local_status(s, now_ms()), &[])
}
