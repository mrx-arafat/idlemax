//! Delivery: `idlemax service install|uninstall|status`. Writes the unit file and runs the
//! launchctl or systemctl commands that `idlemax_core::install::service` plans.
//! Nothing is asked of the service manager for a unit that is not installed, so a run under a
//! temporary HOME never touches the real user's services.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use idlemax_core::install::service::{
    bin_warning, install_steps, parse_status, removal, render, status_step, unit_path, Platform,
    Probe, ServiceEnv, ServiceKind, ServiceStatus, Step, PASSTHROUGH_VARS,
};
use idlemax_core::install::setup::{unit_change, UnitChange};
use idlemax_core::types::Os;
use idlemax_core::Paths;
use serde::Serialize;

use super::files::write_atomic;
use super::out::{hints, toon::Node};
use super::Out;
use text::{install_text, status_text, uninstall_text};

#[derive(Args, Debug)]
pub struct ServiceArgs {
    #[command(subcommand)]
    cmd: ServiceCmd,
}

#[derive(Subcommand, Debug)]
enum ServiceCmd {
    /// Render a Linux root unit offline; nothing is written or started (use --format table for raw unit text)
    Render {
        which: Option<Unit>,
        #[arg(long, value_name = "PATH")]
        bin: Option<PathBuf>,
        /// Target CPU count; required with --memory-mib for off-host rendering
        #[arg(long, requires = "memory_mib", value_parser = clap::value_parser!(u32).range(1..))]
        cpus: Option<u32>,
        /// Target total RAM in MiB
        #[arg(long, requires = "cpus", value_parser = clap::value_parser!(u64).range(1..))]
        memory_mib: Option<u64>,
        /// Target server config; explicit facts otherwise use server defaults
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Target state directory (explicit facts default to /var/lib/idlemax)
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
    },
    /// Write the service file for this OS and start it; running it again restarts it if the file changed
    Install {
        /// Which service [default: agent]
        which: Option<Unit>,
        /// Absolute path of the idlemax binary the service runs [default: this binary]
        #[arg(long, value_name = "PATH")]
        bin: Option<PathBuf>,
        /// Print the file and the commands without writing or running anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop the service and delete its file [default: every installed service]
    Uninstall {
        which: Option<Unit>,
        /// Print the commands without running them or deleting anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Show whether each service is installed, loaded and running
    Status { which: Option<Unit> },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Unit {
    Agent,
    Coord,
}

impl From<Unit> for ServiceKind {
    fn from(u: Unit) -> Self {
        match u {
            Unit::Agent => ServiceKind::Agent,
            Unit::Coord => ServiceKind::Coord,
        }
    }
}

/// Output of one external command.
pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs the planned commands; a fake stands in for it in tests.
pub trait Runner {
    fn run(&self, step: &Step) -> std::io::Result<Output>;
}

pub(super) struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, step: &Step) -> std::io::Result<Output> {
        let (program, args) = step
            .argv
            .split_first()
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        let out = Command::new(program).args(args).output()?;
        Ok(Output {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

mod target;
mod text;

pub(super) struct Ctx {
    pub(super) platform: Platform,
    pub(super) env: ServiceEnv,
}

#[derive(Serialize)]
pub(super) struct InstallReport {
    pub(super) kind: ServiceKind,
    dry_run: bool,
    pub(super) file: PathBuf,
    content: String,
    pub(super) commands: Vec<Step>,
    pub(super) notes: Vec<String>,
}

#[derive(Serialize)]
struct UninstallReport {
    kind: ServiceKind,
    dry_run: bool,
    file: PathBuf,
    /// False when the service was not installed, so nothing was done.
    installed: bool,
    commands: Vec<Step>,
}

pub fn run(paths: &Paths, args: ServiceArgs, out: &Out) -> Result<()> {
    let platform = if matches!(args.cmd, ServiceCmd::Render { .. }) {
        Platform::SystemdUser
    } else {
        Platform::for_os(Os::current())
            .context("`idlemax service` supports macOS and Linux only")?
    };
    let bin = match &args.cmd {
        ServiceCmd::Install { bin, .. } => bin.clone(),
        _ => None,
    };
    let mut env = service_env(paths, platform, bin)?;
    if matches!(args.cmd, ServiceCmd::Install { .. }) {
        size_service(paths, platform, &mut env)?;
    }
    let ctx = Ctx { platform, env };
    match args.cmd {
        ServiceCmd::Render {
            which,
            bin,
            cpus,
            memory_mib,
            config,
            state_dir,
        } => {
            let kind = which.map(ServiceKind::from).unwrap_or(ServiceKind::Agent);
            let unit = target::render(
                paths,
                ctx.env,
                kind,
                bin,
                cpus.zip(memory_mib),
                config,
                state_dir,
            )?;
            out.emit(&unit, |u| u.content.clone(), &[])?;
        }
        ServiceCmd::Install { which, dry_run, .. } => {
            let kinds = kinds(which, &[ServiceKind::Agent]);
            let reports = install(&ctx, paths, &SystemRunner, &kinds, dry_run)?;
            out.emit_with(
                &reports,
                |r| r.iter().map(install_text).collect::<Vec<_>>().join("\n"),
                || named(&reports),
                &hints::then("idlemax service status", "check it"),
            )?;
        }
        ServiceCmd::Uninstall { which, dry_run } => {
            let reports = uninstall(
                &ctx,
                &SystemRunner,
                &kinds(which, &ServiceKind::ALL),
                dry_run,
            )?;
            out.emit_with(
                &reports,
                |r| r.iter().map(uninstall_text).collect::<Vec<_>>().join("\n"),
                || named(&reports),
                &hints::then("idlemax service status", "check it"),
            )?;
        }
        ServiceCmd::Status { which } => {
            let all = status(&ctx, &SystemRunner, &kinds(which, &ServiceKind::ALL));
            out.emit_with(&all, |s| status_text(s), || named(&all), &[])?;
        }
    }
    Ok(())
}

/// The per-service reports under a name, so the TOON reply is not a bare root array.
fn named<T: Serialize>(reports: &[T]) -> Result<Node> {
    Ok(Node::obj([("services", Node::of(&reports)?)]))
}

fn kinds(which: Option<Unit>, default: &[ServiceKind]) -> Vec<ServiceKind> {
    which.map_or_else(|| default.to_vec(), |u| vec![u.into()])
}

fn var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

fn absolute_str(p: &Path) -> Result<String> {
    let abs = std::path::absolute(p).with_context(|| format!("resolving {}", p.display()))?;
    abs.into_os_string()
        .into_string()
        .map_err(|p| anyhow::anyhow!("{} is not valid UTF-8", Path::new(&p).display()))
}

pub(super) fn service_env(
    paths: &Paths,
    platform: Platform,
    bin: Option<PathBuf>,
) -> Result<ServiceEnv> {
    let home = var("HOME")
        .map(PathBuf::from)
        .context("HOME is unset or empty; cannot find where the service file goes")?;
    let bin = match bin {
        Some(b) if !b.is_file() => bail!("--bin {}: no such file", b.display()),
        Some(b) => std::path::absolute(b)?,
        None => std::env::current_exe().context("cannot locate this binary; pass --bin")?,
    };
    let state_dir = PathBuf::from(absolute_str(paths.root())?);
    let mut vars = Vec::new();
    for name in PASSTHROUGH_VARS {
        let value = match (name, var(name)) {
            ("IDLEMAX_HOME", Some(_)) => Some(state_dir.to_string_lossy().into_owned()),
            (_, Some(v)) => Some(absolute_str(Path::new(&v))?),
            (_, None) => None,
        };
        vars.extend(value.map(|v| (name.to_string(), v)));
    }
    Ok(ServiceEnv {
        agent_jobs: 1,
        agent_ram_mib: 512,
        bin,
        home,
        state_dir,
        path: var("PATH").map(|p| p.to_string_lossy().into_owned()),
        vars,
        uid: current_uid(platform)?,
    })
}

/// Gather sizing inputs only for writing/rendering a unit, never for status or removal.
pub(super) fn size_service(paths: &Paths, platform: Platform, env: &mut ServiceEnv) -> Result<()> {
    let config = crate::state::load_config(paths)?;
    let mut sys = sysinfo::System::new().context("reading host capacity")?;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let os = if platform == Platform::SystemdUser {
        Os::Linux
    } else {
        Os::current()
    };
    let policy = config.guard_policy_for(os, sys.cpus().len() as u32, sys.total_memory())?;
    env.agent_jobs = policy.max_jobs;
    env.agent_ram_mib = policy.ram_budget_mib;
    Ok(())
}

/// Numeric user id for launchd's `gui/<uid>` domain. `id -u` avoids a libc binding; systemd's
/// user manager does not need it.
fn current_uid(platform: Platform) -> Result<u32> {
    if platform != Platform::Launchd {
        return Ok(0);
    }
    let out = Command::new("id")
        .arg("-u")
        .output()
        .context("running `id -u`")?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .context("`id -u` did not print a number")
}

pub(super) fn install(
    ctx: &Ctx,
    paths: &Paths,
    runner: &dyn Runner,
    kinds: &[ServiceKind],
    dry_run: bool,
) -> Result<Vec<InstallReport>> {
    let mut reports = Vec::new();
    for &kind in kinds {
        let unit = render(ctx.platform, kind, &ctx.env)?;
        let existing = std::fs::read_to_string(&unit.path).ok();
        let change = unit_change(existing.as_deref(), &unit.content);
        let commands = install_steps(ctx.platform, kind, &ctx.env, change);
        if !dry_run {
            if ctx.platform == Platform::Launchd {
                // launchd opens the log file before the agent starts and gives up if the dir is missing.
                // It may also create the state dir, which holds the token: owner only.
                crate::state::create_private_dir(&paths.logs_dir())
                    .with_context(|| format!("creating {}", paths.logs_dir().display()))?;
            }
            if let Some(dir) = unit.path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            if change != UnitChange::Same {
                write_atomic(&unit.path, unit.content.as_bytes(), Some(0o644))
                    .with_context(|| format!("writing {}", unit.path.display()))?;
            }
            run_steps(runner, &commands, &unit.path, File::InPlace)?;
        }
        reports.push(InstallReport {
            kind,
            dry_run,
            file: unit.path,
            content: unit.content,
            commands,
            notes: install_notes(ctx, paths, kind),
        });
    }
    Ok(reports)
}

fn install_notes(ctx: &Ctx, paths: &Paths, kind: ServiceKind) -> Vec<String> {
    let mut notes: Vec<String> = bin_warning(&ctx.env.bin)
        .map(|w| format!("warning: {w}"))
        .into_iter()
        .collect();
    if kind == ServiceKind::Agent && !paths.token_file().exists() {
        notes.push(format!(
            "warning: no token at {}; run `idlemax init` first, the agent exits without one",
            paths.token_file().display()
        ));
    }
    if ctx.platform == Platform::SystemdUser {
        notes.push("it runs while you are logged in; to run it from boot, use `loginctl enable-linger $USER`".into());
    }
    notes
}

/// What the unit file is like when a step fails, which decides what the error tells you.
#[derive(Clone, Copy)]
enum File {
    InPlace,
    Deleted,
}

fn run_steps(runner: &dyn Runner, steps: &[Step], file: &Path, state: File) -> Result<()> {
    for step in steps {
        let failure = match runner.run(step) {
            Ok(out) if out.ok => continue,
            Ok(out) => out.stderr.trim().to_string(),
            Err(e) => e.to_string(),
        };
        if step.ignore_failure {
            continue;
        }
        let file = file.display();
        match state {
            File::InPlace => bail!(
                "`{step}` failed: {failure}\nthe service file {file} is in place; fix the problem and run the command again"
            ),
            File::Deleted => bail!(
                "`{step}` failed: {failure}\nthe service file {file} is already deleted; fix the problem and run `{step}` yourself"
            ),
        }
    }
    Ok(())
}

fn uninstall(
    ctx: &Ctx,
    runner: &dyn Runner,
    kinds: &[ServiceKind],
    dry_run: bool,
) -> Result<Vec<UninstallReport>> {
    let mut reports = Vec::new();
    for &kind in kinds {
        let file = unit_path(ctx.platform, kind, &ctx.env.home);
        let installed = file.exists();
        let mut commands = Vec::new();
        if installed {
            let steps = removal(ctx.platform, kind, &ctx.env);
            commands = [steps.before.clone(), steps.after.clone()].concat();
            if !dry_run {
                // A service that is not loaded has nothing to stop; any other failure to stop
                // it keeps the file, because deleting it would leave the service running.
                if status(ctx, runner, &[kind])[0].loaded != Some(false) {
                    run_steps(runner, &steps.before, &file, File::InPlace)?;
                }
                std::fs::remove_file(&file)
                    .with_context(|| format!("deleting {}", file.display()))?;
                run_steps(runner, &steps.after, &file, File::Deleted)?;
            }
        }
        reports.push(UninstallReport {
            kind,
            dry_run,
            file,
            installed,
            commands,
        });
    }
    Ok(reports)
}

pub(super) fn status(ctx: &Ctx, runner: &dyn Runner, kinds: &[ServiceKind]) -> Vec<ServiceStatus> {
    kinds
        .iter()
        .map(|&kind| {
            let file = unit_path(ctx.platform, kind, &ctx.env.home);
            if !file.exists() {
                return parse_status(ctx.platform, kind, file, Probe::NotInstalled);
            }
            let step = status_step(ctx.platform, kind, &ctx.env);
            let tool = step.argv.first().cloned().unwrap_or_default();
            match runner.run(&step) {
                Ok(o) => parse_status(
                    ctx.platform,
                    kind,
                    file,
                    Probe::Ran {
                        ok: o.ok,
                        stdout: &o.stdout,
                        stderr: &o.stderr,
                    },
                ),
                Err(_) => parse_status(ctx.platform, kind, file, Probe::Missing(&tool)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
