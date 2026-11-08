//! Delivery: `idlemax setup` runs init, the service install, the optional statusline wiring
//! and doctor in one command. It decides nothing itself: `idlemax_core::install::setup` plans,
//! the existing commands do the work, and this module reports every step as done, already
//! done (a no-op), skipped or failed, with the command that does the same by hand.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use idlemax_core::doctor::Report;
use idlemax_core::install::service::{Platform, ServiceKind};
use idlemax_core::install::setup::{
    plan, Existing, Request, Role, SetupError, SetupPlan, StatuslinePlan,
};
use idlemax_core::types::Os;
use idlemax_core::{Config, Paths};

use super::service::{self, Ctx, SystemRunner};
use super::{job::usage_error, statusline, ExitWith, Out};
use crate::state;
use report::{finish, Status, StepReport};
use steps::{execute, Env};

mod report;
mod steps;
mod text;

#[cfg(test)]
mod tests;

/// How long to wait for freshly installed services to open their ports before doctor looks.
const LISTEN_WAIT: Duration = Duration::from_secs(15);

#[derive(Args, Debug)]
pub struct SetupArgs {
    /// What this device is for: agent runs jobs for a coordinator elsewhere, coord holds the
    /// queue, both does everything on one machine
    #[arg(long, value_enum, default_value_t = RoleArg::Both)]
    role: RoleArg,
    /// Address agents and the CLI use to reach the coordinator, e.g. http://100.64.0.1:7878
    /// (required for --role agent; a coordinator listens there too)
    #[arg(long, value_name = "URL")]
    coord_url: Option<String>,
    /// Read the coordinator's token from stdin instead of generating one (required for --role agent)
    #[arg(long)]
    token_stdin: bool,
    /// Also wire Claude Code's statusline into the quota probe (agent and both)
    #[arg(long)]
    statusline: bool,
    /// Do not install the background service
    #[arg(long)]
    no_service: bool,
    /// Print every file and command without writing or running anything
    #[arg(long)]
    dry_run: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum RoleArg {
    Agent,
    Coord,
    Both,
}

impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Self {
        match r {
            RoleArg::Agent => Role::Agent,
            RoleArg::Coord => Role::Coord,
            RoleArg::Both => Role::Both,
        }
    }
}

pub async fn run(paths: &Paths, args: SetupArgs, coord: Option<String>, out: &Out) -> Result<()> {
    let token = args.token_stdin.then(super::read_token_stdin).transpose()?;
    let request = Request {
        role: args.role.into(),
        os: Os::current(),
        coord_url: args.coord_url.as_deref(),
        token_given: token.is_some(),
        service: !args.no_service,
        statusline: args.statusline,
    };
    let plan = match plan_setup(paths, &request)? {
        Ok(plan) => plan,
        Err(e) => return Err(usage_error(&e.to_string())),
    };
    let env = Env {
        paths,
        svc: service_ctx(paths, &plan)?,
        places: (plan.statusline == StatuslinePlan::Install)
            .then(|| statusline::places(paths))
            .transpose()?,
        runner: &SystemRunner,
        dry_run: args.dry_run,
        token,
    };
    let mut steps = execute(&plan, &env);
    let doctor = doctor_step(&plan, &env, &steps, coord).await;
    steps.push(doctor.0);
    let report = finish(&plan, args.dry_run, steps, doctor.1);
    let next: Vec<String> = report
        .next
        .iter()
        .map(|n| format!("Run `{n}` to continue"))
        .collect();
    out.emit_with(&report, text::render, || Ok(text::view(&report)), &next)?;
    if report.ok {
        Ok(())
    } else {
        Err(ExitWith(1).into())
    }
}

/// Reads what exists on disk and plans. The outer error is a broken `config.toml`; the inner
/// one is a flag that cannot work.
fn plan_setup(paths: &Paths, req: &Request<'_>) -> Result<Result<SetupPlan, SetupError>> {
    let config = existing_config(paths)?;
    let existing = Existing {
        config: config.as_ref(),
        token: paths.token_file().exists(),
    };
    Ok(plan(req, &existing))
}

fn existing_config(paths: &Paths) -> Result<Option<Config>> {
    if !paths.config_file().exists() {
        return Ok(None);
    }
    let config = state::load_config(paths)
        .context("config.toml does not parse: fix it or move it aside, then run setup again")?;
    Ok(Some(config))
}

fn service_ctx(paths: &Paths, plan: &SetupPlan) -> Result<Option<Ctx>> {
    if plan.services.is_empty() {
        return Ok(None);
    }
    let platform = Platform::for_os(Os::current())
        .context("`idlemax setup` installs services on macOS and Linux only; use --no-service")?;
    let env = service::service_env(paths, platform, None)?;
    Ok(Some(Ctx { platform, env }))
}

/// The last step. It waits for services this run just started, then asks doctor.
async fn doctor_step(
    plan: &SetupPlan,
    env: &Env<'_>,
    steps: &[StepReport],
    coord: Option<String>,
) -> (StepReport, Option<Report>) {
    let cmd = "idlemax doctor".to_string();
    if let Some(failed) = steps.iter().find(|s| s.status == Status::Failed) {
        let why = format!("not run: the {} step failed", failed.step);
        return (StepReport::new("doctor", Status::Skipped, why, cmd), None);
    }
    if env.dry_run {
        let why = "checks the whole setup once it is written";
        let mut step = StepReport::new("doctor", Status::WouldDo, why, cmd.clone());
        step.runs = vec![cmd];
        return (step, None);
    }
    let started_services = steps
        .iter()
        .any(|s| s.step == "service" && s.status == Status::Done);
    if started_services {
        wait_until_listening(env.paths, plan).await;
    }
    let report = crate::doctor::run(coord).await;
    let (status, c) = (
        if report.healthy {
            Status::Done
        } else {
            Status::Problems
        },
        &report.counts,
    );
    let detail = format!(
        "{} checks: {} ok, {} warn, {} fail",
        report.checks.len(),
        c.ok,
        c.warn,
        c.fail
    );
    (StepReport::new("doctor", status, detail, cmd), Some(report))
}

/// Gives the services this run installed a moment to open their ports.
async fn wait_until_listening(paths: &Paths, plan: &SetupPlan) {
    let Ok(config) = state::load_config(paths) else {
        return;
    };
    let addrs: Vec<String> = plan
        .services
        .iter()
        .map(|kind| match kind {
            ServiceKind::Coord => config.coordinator.listen.clone(),
            ServiceKind::Agent => config.agent.listen.clone(),
        })
        .collect();
    let deadline = Instant::now() + LISTEN_WAIT;
    for addr in addrs {
        while Instant::now() < deadline {
            let connect = tokio::net::TcpStream::connect(addr.as_str());
            if matches!(
                tokio::time::timeout(Duration::from_millis(500), connect).await,
                Ok(Ok(_))
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}
