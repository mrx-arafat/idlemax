//! Delivery: runs the parsed command. Commands that need no server run here; the rest get a
//! client for the coordinator and go to `handlers`.

use anyhow::{bail, Result};
use idlemax_core::types::ControlAction;
use idlemax_core::Paths;

use super::commands::{AdviseCmd, Cmd, InboxCmd, JobCmd};
use super::init_cmd::run_init;
use super::{
    config_cmd, handlers, home, hooks, inbox, job, logs, out, print, report, rows, schedule,
    service, setup, skill, statusline, ApiClient, Cli, CliError, ClientError, ExitWith, Out, Peer,
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
        Cmd::Pause(t) => handlers::control(c, t.request(ControlAction::Pause), out).await,
        Cmd::Resume(t) => handlers::control(c, t.request(ControlAction::Resume), out).await,
        Cmd::StopAll(t) => handlers::control(c, t.request(ControlAction::StopAll), out).await,
        Cmd::Quota => handlers::quota(c, out, now).await,
        Cmd::Report(args) => report::run(c, args, out, now, zone).await,
        Cmd::Events { limit } => handlers::events(c, out, limit, zone).await,
        Cmd::Advise { cmd } => handlers::advise(c, cmd.unwrap_or(AdviseCmd::List), out, now).await,
        Cmd::Tui => Ok(crate::tui::run(c.clone(), zone).await?),
        Cmd::Job(JobCmd::Add(_))
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
        | Cmd::Skill(_) => {
            bail!("not a coordinator command")
        }
    }
}
