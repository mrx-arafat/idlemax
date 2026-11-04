//! Delivery: clap command line. Each subcommand makes one API call and prints the result.

mod client;
mod init;
pub mod print;
pub mod rows;

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use idlemax_core::protocol::ControlRequest;
use idlemax_core::types::{ControlAction, ControlScope, DeviceProfile, JobId, JobSpec, JobStatus};
use idlemax_core::Paths;
use serde::Serialize;

use crate::state::{self, now_ms};

pub use client::{ApiClient, ClientError};

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
    /// Print JSON instead of tables
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
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
    /// AI advisor recommendations (default: list)
    Advise {
        #[command(subcommand)]
        cmd: Option<AdviseCmd>,
    },
    /// Interactive dashboard
    Tui,
    /// Talk to this device's agent directly; works while the coordinator is down
    #[command(subcommand)]
    Local(LocalCmd),
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
    /// Queue the job described by a TOML spec file
    Add { spec: PathBuf },
    /// Cancel a job (kills it if running)
    Cancel { id: JobId },
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
    let Cli { coord, json, cmd } = Cli::parse();
    let paths = state::paths_from_env()?;
    let cmd = match cmd {
        Cmd::Init(args) => return run_init(&paths, args),
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
            run_local(cmd, &client, json)
                .await
                .map_err(|e| hint(e, LOCAL_DOWN))
        }
        cmd => {
            let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
            let client = ApiClient::new(&url, token(&paths)?)?;
            run_coord(cmd, &client, json).await
        }
    }
}

fn run_init(paths: &Paths, args: InitArgs) -> Result<()> {
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
    println!("{} {}", word(report.config), paths.config_file().display());
    println!("{} {}", word(report.token), paths.token_file().display());
    Ok(())
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
fn hint(e: ClientError, msg: &'static str) -> anyhow::Error {
    let down = matches!(e, ClientError::Unreachable { .. });
    let e = anyhow::Error::new(e);
    if down {
        e.context(msg)
    } else {
        e
    }
}

async fn run_coord(cmd: Cmd, c: &ApiClient, json: bool) -> Result<()> {
    let now = now_ms();
    let r = match cmd {
        Cmd::Status => c
            .status()
            .await
            .map(|s| out(json, &s, |s| print::status(s, now))),
        Cmd::Jobs { status, limit } => c
            .jobs(status, limit)
            .await
            .map(|j| out(json, &j, |j| print::jobs(j, now))),
        Cmd::Job(JobCmd::Add { spec }) => {
            let spec = read_spec(&spec)?;
            c.add_job(&spec)
                .await
                .map(|j| out(json, &j, print::job_line))
        }
        Cmd::Job(JobCmd::Cancel { id }) => c
            .cancel_job(id)
            .await
            .map(|j| out(json, &j, print::job_line)),
        Cmd::Pause(t) => control(c, t.request(ControlAction::Pause), json).await,
        Cmd::Resume(t) => control(c, t.request(ControlAction::Resume), json).await,
        Cmd::StopAll(t) => control(c, t.request(ControlAction::StopAll), json).await,
        Cmd::Quota => quota(c, json, now).await,
        Cmd::Events { limit } => c
            .events(limit)
            .await
            .map(|e| out(json, &e, |e| print::events(e))),
        Cmd::Advise { cmd } => advise(c, cmd.unwrap_or(AdviseCmd::List), json, now).await,
        Cmd::Tui => return Ok(crate::tui::run(c.clone()).await?),
        Cmd::Agent | Cmd::Coord | Cmd::Init(_) | Cmd::Local(_) => {
            bail!("not a coordinator command")
        }
    };
    r.map_err(|e| hint(e, COORD_DOWN))
}

async fn control(c: &ApiClient, req: ControlRequest, json: bool) -> client::Result<()> {
    let s = c.control(&req).await?;
    out(json, &s, |s| {
        format!("ok: {}\n{}", print::control_label(&req), print::summary(s))
    });
    Ok(())
}

async fn quota(c: &ApiClient, json: bool, now: i64) -> client::Result<()> {
    let (views, status) = tokio::try_join!(c.quota(), c.status())?;
    out(json, &views, |v| {
        print::quota(v, &status.paused_accounts, now)
    });
    Ok(())
}

async fn advise(c: &ApiClient, cmd: AdviseCmd, json: bool, now: i64) -> client::Result<()> {
    match cmd {
        AdviseCmd::List => out(json, &c.advice().await?, |a| print::advice(a, now)),
        AdviseCmd::Refresh => out(json, &c.refresh_advice().await?, |a| print::advice(a, now)),
        AdviseCmd::Accept { id } => out(json, &c.accept_advice(id).await?, print::job_line),
        AdviseCmd::Dismiss { id } => out(json, &c.dismiss_advice(id).await?, print::advice_line),
    }
    Ok(())
}

async fn run_local(cmd: LocalCmd, c: &ApiClient, json: bool) -> client::Result<()> {
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
    out(json, &s, |s| print::local_status(s, now_ms()));
    Ok(())
}

fn read_spec(path: &Path) -> Result<JobSpec> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let spec: JobSpec =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    spec.validate()
        .with_context(|| format!("invalid job spec {}", path.display()))?;
    Ok(spec)
}

/// Prints `value` as pretty JSON or through `text`.
fn out<T: Serialize>(json: bool, value: &T, text: impl FnOnce(&T) -> String) {
    if json {
        match serde_json::to_string_pretty(value) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("idlemax: cannot encode JSON: {e}"),
        }
    } else {
        println!("{}", text(value));
    }
}
