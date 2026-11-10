//! Delivery: the steps of `idlemax setup`. Each one asks the existing code to do the work (init,
//! service install, statusline install) and reports what happened.

use anyhow::{Context, Result};
use idlemax_core::config::Edit;
use idlemax_core::install::service::{render, ServiceKind};
use idlemax_core::install::setup::{
    unit_change, ConfigPlan, SetupPlan, StatuslinePlan, TokenPlan, UnitChange,
};
use idlemax_core::{Config, Paths, DEFAULT_COORD_LISTEN, DEFAULT_COORD_URL};

use super::report::{Status, StepReport};
use crate::cli::service::{self, Ctx, Runner};
use crate::cli::{init, rows, statusline};
use crate::state;

/// Everything the steps need besides the plan.
pub(super) struct Env<'a> {
    pub(super) paths: &'a Paths,
    /// Set when services are to be installed.
    pub(super) svc: Option<Ctx>,
    /// Set when the statusline is to be wired.
    pub(super) places: Option<statusline::Places>,
    pub(super) runner: &'a dyn Runner,
    pub(super) dry_run: bool,
    /// The token piped in with `--token-stdin`.
    pub(super) token: Option<String>,
    pub(super) allow: Option<&'a Edit>,
}

enum Stage {
    Config,
    Token,
    Allowlist,
    Service(ServiceKind),
    ServiceSkipped(&'static str),
    Statusline(StatuslinePlan),
}

fn stages(plan: &SetupPlan, allow: bool) -> Vec<Stage> {
    let mut stages = vec![Stage::Config, Stage::Token];
    if allow {
        stages.push(Stage::Allowlist);
    }
    match plan.service_skipped {
        Some(reason) => stages.push(Stage::ServiceSkipped(reason)),
        None => stages.extend(plan.services.iter().map(|k| Stage::Service(*k))),
    }
    stages.push(Stage::Statusline(plan.statusline));
    stages
}

/// Runs the steps in order. After a failure the rest are reported as not run.
pub(super) fn execute(plan: &SetupPlan, env: &Env<'_>) -> Vec<StepReport> {
    let mut steps: Vec<StepReport> = Vec::new();
    let mut broke: Option<&'static str> = None;
    for stage in stages(plan, env.allow.is_some()) {
        let report = match broke {
            None => run_stage(&stage, plan, env),
            Some(by) => not_run(&stage, plan, by),
        };
        if report.status == Status::Failed {
            broke = Some(report.step);
        }
        steps.push(report);
    }
    steps
}

fn run_stage(stage: &Stage, plan: &SetupPlan, env: &Env<'_>) -> StepReport {
    match stage {
        Stage::Config => config_step(plan, env),
        Stage::Token => token_step(plan, env),
        Stage::Allowlist => match env.allow {
            Some(edit) => super::allowlist::step(env.paths, edit, env.dry_run),
            None => skipped(stage, plan, "no --allow value"),
        },
        Stage::Service(kind) => service_step(*kind, plan, env),
        Stage::ServiceSkipped(reason) => skipped(stage, plan, reason),
        Stage::Statusline(StatuslinePlan::Install) => statusline_step(plan, env),
        Stage::Statusline(StatuslinePlan::Skip(reason)) => skipped(stage, plan, reason),
    }
}

fn skipped(stage: &Stage, plan: &SetupPlan, why: &str) -> StepReport {
    let (step, which) = labels(stage);
    let mut report = StepReport::new(step, Status::Skipped, why, command(stage, plan));
    report.which = which;
    report
}

fn not_run(stage: &Stage, plan: &SetupPlan, by: &str) -> StepReport {
    skipped(stage, plan, &format!("not run: the {by} step failed"))
}

fn labels(stage: &Stage) -> (&'static str, Option<&'static str>) {
    match stage {
        Stage::Config => ("config", None),
        Stage::Token => ("token", None),
        Stage::Allowlist => ("allowlist", None),
        Stage::Service(kind) => ("service", Some(kind.name())),
        Stage::ServiceSkipped(_) => ("service", None),
        Stage::Statusline(_) => ("statusline", None),
    }
}

/// The command that does one step by hand.
fn command(stage: &Stage, plan: &SetupPlan) -> String {
    match stage {
        // One `init` makes both files, so the token on stdin belongs to both steps.
        Stage::Config | Stage::Token => init_command(plan, plan.token == TokenPlan::FromStdin),
        Stage::Service(kind) => format!("idlemax service install {}", kind.name()),
        Stage::ServiceSkipped(_) => "idlemax service install".into(),
        Stage::Statusline(_) => "idlemax statusline install".into(),
        Stage::Allowlist => "idlemax config set agent.command_allowlist <programs>".into(),
    }
}

fn init_command(plan: &SetupPlan, token_stdin: bool) -> String {
    let mut cmd = "idlemax init".to_string();
    if plan.coord_url != DEFAULT_COORD_URL {
        cmd.push_str(&format!(" --coord-url {}", plan.coord_url));
    }
    if token_stdin {
        return format!("<the coordinator's token> | {cmd} --token-stdin");
    }
    cmd
}

fn config_step(plan: &SetupPlan, env: &Env<'_>) -> StepReport {
    let path = env.paths.config_file();
    let cmd = command(&Stage::Config, plan);
    let ConfigPlan::Write(cfg) = &plan.config else {
        let detail = format!(
            "{} exists and is left as it is (coordinator.url {})",
            path.display(),
            plan.coord_url
        );
        return StepReport::new("config", Status::AlreadyDone, detail, cmd);
    };
    let profile = cfg
        .agent
        .profile
        .map_or_else(String::new, |p| rows::snake(&p));
    let detail = format!(
        "coordinator.url {}, coordinator.listen {}, agent.profile {profile}",
        cfg.coordinator.url, cfg.coordinator.listen
    );
    let mut notes = Vec::new();
    if cfg.coordinator.listen != DEFAULT_COORD_LISTEN {
        notes.push(format!(
            "the by-hand command leaves coordinator.listen at its default: add listen = \"{}\" under [coordinator]",
            cfg.coordinator.listen
        ));
    }
    let done = |status, detail: String| {
        StepReport::new("config", status, detail, cmd.clone())
            .with_files(vec![path.clone()])
            .with_notes(notes.clone())
    };
    if env.dry_run {
        return done(Status::WouldDo, detail);
    }
    match write_config(env.paths, cfg) {
        Ok(()) => done(Status::Done, detail),
        Err(e) => StepReport::new("config", Status::Failed, format!("{e:#}"), cmd),
    }
}

fn write_config(paths: &Paths, cfg: &Config) -> Result<()> {
    state::ensure_dirs(paths)?;
    init::write_new_config(paths, cfg)
}

fn token_step(plan: &SetupPlan, env: &Env<'_>) -> StepReport {
    let path = env.paths.token_file();
    let cmd = command(&Stage::Token, plan);
    let fail = |detail: String| StepReport::new("token", Status::Failed, detail, cmd.clone());
    let done = |status, detail: String| {
        StepReport::new("token", status, detail, cmd.clone()).with_files(vec![path.clone()])
    };
    let stdin = env.token.as_deref();
    if plan.token == TokenPlan::Keep {
        let detail = format!("{} exists and is left as it is", path.display());
        let same = stdin.map(|given| same_token(&path, given));
        return match same {
            Some(Err(e)) => fail(format!("{e:#}")),
            Some(Ok(false)) => fail(format!(
                "{} holds a different token than the one on stdin: remove the file to replace it",
                path.display()
            )),
            _ => StepReport::new("token", Status::AlreadyDone, detail, cmd),
        };
    }
    let how = match plan.token {
        TokenPlan::FromStdin => "the token from stdin",
        _ => "a new random token",
    };
    let detail = format!("{how}, mode 0600");
    if env.dry_run {
        return done(Status::WouldDo, detail);
    }
    let written = state::ensure_dirs(env.paths)
        .map_err(anyhow::Error::from)
        .and_then(|()| init::write_token_file(env.paths, stdin));
    match written {
        Ok(_) => done(Status::Done, detail),
        Err(e) => fail(format!("{e:#}")),
    }
}

/// Whether the token file holds `given`.
fn same_token(path: &std::path::Path, given: &str) -> Result<bool> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text.trim() == given.trim())
}

fn service_step(kind: ServiceKind, plan: &SetupPlan, env: &Env<'_>) -> StepReport {
    let stage = Stage::Service(kind);
    let cmd = command(&stage, plan);
    let report = |status, detail: String| {
        let mut r = StepReport::new("service", status, detail, cmd.clone());
        r.which = Some(kind.name());
        r
    };
    let Some(ctx) = &env.svc else {
        return report(Status::Failed, "no service support on this OS".into());
    };
    let unit = match render(ctx.platform, kind, &ctx.env) {
        Ok(unit) => unit,
        Err(e) => return report(Status::Failed, e.to_string()),
    };
    let existing = std::fs::read_to_string(&unit.path).ok();
    let how = match unit_change(existing.as_deref(), &unit.content) {
        UnitChange::Same if is_running(ctx, env, kind) => {
            let detail = format!("{} is current and running", unit.path.display());
            return report(Status::AlreadyDone, detail);
        }
        // A first run whose start failed leaves the right file and no running service.
        UnitChange::Same => "current, but not running: started",
        UnitChange::Create => "new, then started",
        UnitChange::Update => "changed, then restarted",
    };
    let detail = how.to_string();
    match service::install(ctx, env.paths, env.runner, &[kind], env.dry_run) {
        Ok(mut done) => {
            let Some(install) = done.pop() else {
                return report(
                    Status::Failed,
                    "the service install reported nothing".into(),
                );
            };
            let status = if env.dry_run {
                Status::WouldDo
            } else {
                Status::Done
            };
            let mut r = report(status, detail).with_files(vec![install.file]);
            r.runs = install.commands.iter().map(ToString::to_string).collect();
            r.notes = install_notes(install.notes, env, plan);
            r
        }
        Err(e) => report(Status::Failed, format!("{e:#}")),
    }
}

/// Whether the service manager reports the service loaded and running. Read-only, and it
/// asks nothing for a unit that is not installed.
fn is_running(ctx: &Ctx, env: &Env<'_>, kind: ServiceKind) -> bool {
    let state = service::status(ctx, env.runner, &[kind]);
    state
        .first()
        .is_some_and(|s| s.loaded == Some(true) && s.running == Some(true))
}

/// A dry run has not made the token yet, so the install's "no token" warning would mislead.
fn install_notes(notes: Vec<String>, env: &Env<'_>, plan: &SetupPlan) -> Vec<String> {
    let token_comes = env.dry_run && plan.token != TokenPlan::Keep;
    notes
        .into_iter()
        .filter(|n| !(token_comes && n.contains("no token at")))
        .collect()
}

fn statusline_step(plan: &SetupPlan, env: &Env<'_>) -> StepReport {
    let cmd = command(&Stage::Statusline(plan.statusline), plan);
    let fail = |detail: String| StepReport::new("statusline", Status::Failed, detail, cmd.clone());
    let Some(places) = &env.places else {
        return fail("no place to find Claude Code's settings.json".into());
    };
    let change = match statusline::install(places, env.dry_run) {
        Ok(change) => change,
        Err(e) => return fail(format!("{e:#}")),
    };
    if !change.changed {
        let detail = format!(
            "{} already wraps the statusLine command",
            change.settings.display()
        );
        return StepReport::new("statusline", Status::AlreadyDone, detail, cmd);
    }
    let status = if env.dry_run {
        Status::WouldDo
    } else {
        Status::Done
    };
    let mut notes = change.notes;
    notes.extend(
        change
            .backup
            .map(|b| format!("backup of the old settings.json: {}", b.display())),
    );
    let detail = "wraps Claude Code's statusLine command".to_string();
    StepReport::new("statusline", status, detail, cmd)
        .with_files(vec![change.settings, change.wrapper])
        .with_notes(notes)
}
