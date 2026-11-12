//! Delivery: runs the parsed command. Commands that need no server run here; the rest get a
//! client for the coordinator and go to `handlers`.

use anyhow::{bail, Result};
use idlemax_core::control::caller::{Access, CallerError, LAUNCHED_ENV};
use idlemax_core::types::ControlAction;
use idlemax_core::Paths;

use super::commands::{AdviseCmd, Cmd, InboxCmd, JobCmd, LocalCmd};
use super::device::DeviceCmd;
use super::init_cmd::run_init;
use super::out::OWNER_ONLY_HELP;
use super::project::ProjectCmd;
use super::{
    config_cmd, device, handlers, home, hooks, inbox, job, logs, mcp, mode, out, plan, print,
    project, report, review, rows, schedule, service, setup, skill, statusline, workers, ApiClient,
    Cli, CliError, ClientError, ExitWith, Out, Peer,
};
use crate::state::{self, now_ms};

pub(super) async fn execute(cli: Cli, out: &Out) -> Result<()> {
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
    // Before the token is loaded and before any call: a launched job or worker gets the
    // lock's own text, not a token or network error.
    if access_of(&cmd) == Access::Owner && std::env::var_os(LAUNCHED_ENV).is_some() {
        let why = CallerError::LaunchedHere.to_string();
        return Err(CliError::runtime(why, OWNER_ONLY_HELP).into());
    }
    // Doctor and the hooks report a broken state dir or config instead of failing on it.
    let cmd = match cmd {
        Cmd::Doctor => return run_doctor(coord, out).await,
        Cmd::Hooks(args) => return hooks::run(coord, args, out).await,
        Cmd::Skill(args) => return skill::run(args, out),
        // Before the state dir: the handshake must work on a device that is not set up.
        Cmd::Mcp => return mcp::run(coord).await,
        other => other,
    };
    let paths = state::paths_from_env()?;
    let cmd = match cmd {
        Cmd::Init(args) => return run_init(&paths, args, out),
        Cmd::Config(args) => return config_cmd::run(&paths, args, out).await,
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
        Cmd::Review(args) => {
            let url = coord.unwrap_or_else(|| config.coordinator.url.clone());
            let client = ApiClient::new(&url, token(&paths)?)?;
            review::run(args, &client, &paths, out)
                .await
                .map_err(|e| hint(e, Peer::Coordinator))
        }
        Cmd::Logs { id, tail, follow } => {
            let log_path = paths.job_log_file(id);
            if log_path.exists() {
                logs::run_logs_local(&log_path, tail, follow, out).await
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

/// What a command needs from its caller (docs/API.md, the access table). One match with no
/// wildcard arm, so a command added later must be classed. Token-only commands are `Read`.
fn access_of(cmd: &Cmd) -> Access {
    match cmd {
        Cmd::Resume(_)
        | Cmd::Start
        | Cmd::Local(LocalCmd::Resume)
        | Cmd::Device(DeviceCmd::On { .. })
        | Cmd::Project(ProjectCmd::On { .. } | ProjectCmd::Add { .. } | ProjectCmd::Set { .. }) => {
            Access::Owner
        }
        Cmd::Config(args) if args.changes_settings() => Access::Owner,
        Cmd::Plan(args) if args.refresh => Access::Owner,
        Cmd::Review(args) if args.acts() => Access::Owner,
        Cmd::Mode(args) if !args.switches_off() => Access::Owner,
        Cmd::Mode(_)
        | Cmd::Stop(_)
        | Cmd::Pause(_)
        | Cmd::StopAll(_)
        | Cmd::Local(LocalCmd::Pause | LocalCmd::StopAll)
        | Cmd::Project(ProjectCmd::Off { .. })
        | Cmd::Device(DeviceCmd::Off { .. })
        | Cmd::Job(JobCmd::Cancel { .. }) => Access::Stop,
        Cmd::Config(_)
        | Cmd::Local(LocalCmd::Status)
        | Cmd::Project(ProjectCmd::List { .. })
        | Cmd::Device(DeviceCmd::List)
        | Cmd::Job(JobCmd::Add(_))
        | Cmd::Agent
        | Cmd::Coord
        | Cmd::Init(_)
        | Cmd::Status
        | Cmd::Jobs { .. }
        | Cmd::Schedule(_)
        | Cmd::Why { .. }
        | Cmd::Quota
        | Cmd::Plan(_)
        | Cmd::Review(_)
        | Cmd::Report(_)
        | Cmd::Events { .. }
        | Cmd::Logs { .. }
        | Cmd::Inbox { .. }
        | Cmd::Advise { .. }
        | Cmd::Setup(_)
        | Cmd::Service(_)
        | Cmd::Statusline(_)
        | Cmd::Tui
        // Its tools carry their own check.
        | Cmd::Mcp
        | Cmd::Doctor
        | Cmd::Hooks(_)
        | Cmd::Skill(_) => Access::Read,
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

pub(super) fn token(paths: &Paths) -> Result<String> {
    state::load_token(paths).map_err(|e| {
        let help = match &e {
            state::StateError::TokenNotPrivate(path) => format!(
                "chmod 600 {}",
                idlemax_core::install::quote::shell_quote(&path.to_string_lossy())
            ),
            state::StateError::EmptyTokenEnv | state::StateError::InvalidTokenEnv(_) =>
                "unset IDLEMAX_TOKEN to use the token file, or set it to the shared coordinator token".into(),
            state::StateError::Config { path, .. } => format!(
                "put the shared coordinator token in {} (at least 32 characters)",
                path.display()
            ),
            state::StateError::Io { source, .. }
                if source.kind() == std::io::ErrorKind::NotFound =>
                "idlemax init (or set IDLEMAX_TOKEN)".into(),
            _ => "idlemax doctor (check the token path and permissions)".into(),
        };
        CliError::runtime(format!("cannot load API token ({e})"), help).into()
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
pub(super) fn hint(e: impl Into<anyhow::Error>, peer: Peer) -> anyhow::Error {
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
        Cmd::Why { id } => handlers::why(c, id, out).await,
        Cmd::Job(JobCmd::Cancel { id }) => handlers::cancel(c, id, out).await,
        Cmd::Schedule(cmd) => schedule::run(cmd, c, out, now).await,
        Cmd::Project(cmd) => project::run(cmd, c, out).await,
        Cmd::Mode(args) => mode::run(args, c, out).await,
        Cmd::Device(cmd) => device::run(cmd, c, out).await,
        Cmd::Plan(args) => plan::run(args, c, out).await,
        Cmd::Stop(args) => workers::stop(args, c, out).await,
        Cmd::Start => workers::start(c, out).await,
        Cmd::Pause(t) => handlers::control(c, t.request(ControlAction::Pause), out).await,
        Cmd::Resume(t) => handlers::control(c, t.request(ControlAction::Resume), out).await,
        Cmd::StopAll(t) => handlers::control(c, t.request(ControlAction::StopAll), out).await,
        Cmd::Quota => handlers::quota(c, out, now).await,
        Cmd::Report(args) => report::run(c, args, out, now, zone).await,
        Cmd::Events { limit } => handlers::events(c, out, limit, zone).await,
        Cmd::Advise { cmd } => handlers::advise(c, cmd.unwrap_or(AdviseCmd::List), out, now).await,
        Cmd::Tui => Ok(crate::tui::run(c.clone(), zone).await?),
        Cmd::Job(JobCmd::Add(_))
        | Cmd::Review(_)
        | Cmd::Agent
        | Cmd::Coord
        | Cmd::Init(_)
        | Cmd::Config(_)
        | Cmd::Local(_)
        | Cmd::Setup(_)
        | Cmd::Service(_)
        | Cmd::Statusline(_)
        | Cmd::Logs { .. }
        | Cmd::Inbox { .. }
        | Cmd::Doctor
        | Cmd::Hooks(_)
        | Cmd::Skill(_)
        | Cmd::Mcp => {
            bail!("not a coordinator command")
        }
    }
}
