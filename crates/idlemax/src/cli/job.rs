//! Delivery: `idlemax job add`: build the spec from flags, a file or stdin, queue it, and
//! optionally wait for the result.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use idlemax_core::job_draft::{DraftError, JobDraft};
use idlemax_core::types::{CostClass, Job, JobSpec, JobStatus};
use idlemax_core::Paths;

use super::out::{hints, printable, views};
use super::{job_wait, noop, print, schedule, why, ApiClient, CliError, Format, Out};

const NOTHING_TO_RUN: &str = "nothing to run: give a spec file (`idlemax job add spec.toml`), \
    `-` to read the spec from stdin, or a command after `--` (`idlemax job add -- cargo test`)\n";

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Spec file in TOML, or `-` to read the spec from stdin
    #[arg(value_name = "SPEC", conflicts_with = "argv")]
    spec: Option<PathBuf>,
    /// The command and its arguments, after `--`; run directly, never through a shell
    #[arg(last = true, value_name = "ARGV")]
    argv: Vec<String>,
    /// Job name [default: the command's file name]
    #[arg(long, conflicts_with = "spec")]
    name: Option<String>,
    /// Kind of load; ai needs --ai-account [default: cpu_light]
    #[arg(long, value_enum, conflicts_with = "spec")]
    cost: Option<CostArg>,
    /// Run only on this device (repeatable)
    #[arg(long, value_name = "DEVICE", conflicts_with = "spec")]
    device: Vec<String>,
    /// Tool the device must have, such as cargo (repeatable)
    #[arg(long, value_name = "TOOL", conflicts_with = "spec")]
    requires: Vec<String>,
    /// Memory the job may use [default: 512]
    #[arg(long, value_name = "MIB", conflicts_with = "spec")]
    #[arg(value_parser = clap::value_parser!(u64).range(1..))]
    mem: Option<u64>,
    /// Longest the job may run before it is killed [default: 1800]
    #[arg(long, value_name = "SECS", conflicts_with = "spec")]
    #[arg(value_parser = clap::value_parser!(u64).range(1..))]
    max_runtime: Option<u64>,
    /// Higher runs first [default: 0]
    #[arg(long, allow_negative_numbers = true, conflicts_with = "spec")]
    priority: Option<i32>,
    /// AI account to spend; makes this an AI job
    #[arg(long, value_name = "ACCOUNT", conflicts_with = "spec")]
    ai_account: Option<String>,
    /// Directory to run in. A relative path is made absolute here, and must exist on the
    /// device that runs the job
    #[arg(long, value_name = "PATH", conflicts_with = "spec")]
    cwd: Option<String>,
    /// Idempotency key: while a job with this key has not finished, adding it again
    /// returns that job instead of queueing another
    #[arg(long, value_name = "KEY")]
    key: Option<String>,
    /// Wait for the job to finish and print its result; the exit code mirrors the job
    #[arg(long)]
    wait: bool,
    /// Stop waiting after this long and exit 124; the job stays queued
    #[arg(long, value_name = "SECS", requires = "wait")]
    #[arg(value_parser = clap::value_parser!(u64).range(1..))]
    timeout: Option<u64>,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum CostArg {
    #[value(name = "cpu_light", alias = "cpu-light")]
    CpuLight,
    #[value(name = "cpu_heavy", alias = "cpu-heavy")]
    CpuHeavy,
    Io,
    Ai,
}

impl From<CostArg> for CostClass {
    fn from(c: CostArg) -> Self {
        match c {
            CostArg::CpuLight => CostClass::CpuLight,
            CostArg::CpuHeavy => CostClass::CpuHeavy,
            CostArg::Io => CostClass::Io,
            CostArg::Ai => CostClass::Ai,
        }
    }
}

/// The exit code a command reports after it has printed its own result. `main` turns it
/// into the process exit code without printing an error.
#[derive(Debug)]
pub struct ExitWith(pub u8);

impl fmt::Display for ExitWith {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exit code {}", self.0)
    }
}

impl std::error::Error for ExitWith {}

/// `idlemax job add`: queue the job, then wait for it when asked.
pub async fn add(c: &ApiClient, paths: &Paths, args: AddArgs, out: &Out) -> Result<()> {
    let (wait, timeout) = (args.wait, args.timeout.map(Duration::from_secs));
    let spec = args.into_spec()?;
    let (job, created) = c
        .add_job(&spec)
        .await
        .map_err(|e| super::hint(e, super::Peer::Coordinator))?;
    let warning = why::unrunnable_warning(c, &spec).await;
    // TOON carries the warning in the reply, since agents read stdout only.
    if let (Some(w), false) = (&warning, out.is_toon() && !wait) {
        eprintln!("warning: {w}");
    }
    announce(&job, created, out, wait, warning.as_deref())?;
    if wait {
        if let (true, Some(warning)) = (job.status == JobStatus::Queued, warning) {
            return Err(CliError::runtime(
                format!("job {} is queued but cannot start: {warning}", job.id),
                format!(
                    "idlemax why {}; idlemax job cancel {} to remove it",
                    job.id, job.id
                ),
            )
            .into());
        }
        job_wait::run(c, paths, job, timeout, out).await
    } else {
        Ok(())
    }
}

impl AddArgs {
    fn into_spec(self) -> Result<JobSpec> {
        let key = self.key.clone();
        let mut spec = match self.spec.as_deref() {
            Some(p) if p == Path::new("-") => parse_spec(&read_stdin()?, "stdin", "<file>")?,
            Some(p) => read_spec(p)?,
            None if self.argv.is_empty() => return Err(usage_error(NOTHING_TO_RUN)),
            None => {
                let here = std::env::current_dir().context("cannot read the current directory")?;
                return self.draft().into_spec(&here).map_err(|e| match e {
                    DraftError::AccountNeedsAiCost | DraftError::AiCostNeedsAccount => {
                        usage_error(&e.to_string())
                    }
                    other => other.into(),
                });
            }
        };
        if key.is_some() {
            spec.key = key;
            spec.validate().context("invalid --key")?;
        }
        Ok(spec)
    }

    fn draft(self) -> JobDraft {
        JobDraft {
            argv: self.argv,
            name: self.name,
            cost: self.cost.map(Into::into),
            devices: self.device,
            requires: self.requires,
            mem_mb: self.mem,
            max_runtime_secs: self.max_runtime,
            priority: self.priority,
            ai_account: self.ai_account,
            cwd: self.cwd,
            key: self.key,
        }
    }
}

/// A command line mistake: exit 2, with the part after the first ": " as the fix.
pub(super) fn usage_error(message: &str) -> anyhow::Error {
    let message = message.trim_end();
    let (what, help) = message
        .split_once(": ")
        .unwrap_or((message, "idlemax job add --help"));
    CliError::usage(what, help).into()
}

fn read_spec(path: &Path) -> Result<JobSpec> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_spec(
        &text,
        &path.display().to_string(),
        &path.display().to_string(),
    )
}

fn read_stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the spec from stdin")?;
    Ok(text)
}

/// Parses and validates TOML. `origin` names it in errors; `schedule_arg` is what the
/// `every` note tells the user to pass to `schedule add`.
fn parse_spec(text: &str, origin: &str, schedule_arg: &str) -> Result<JobSpec> {
    let spec = parse_file_spec(text, origin)?;
    spec.validate()
        .with_context(|| format!("invalid job spec {origin}"))?;
    if schedule::sets_every(text) {
        eprintln!(
            "note: `every` is ignored by `job add`, so this queues one run; \
             to repeat it, use `idlemax schedule add {schedule_arg}`"
        );
    }
    Ok(spec)
}

/// File input is strict, while HTTP job specs retain their forward-compatible schema.
pub(super) fn parse_file_spec(text: &str, origin: &str) -> Result<JobSpec> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileSpec {
        #[serde(flatten)]
        spec: JobSpec,
        #[serde(rename = "every")]
        _every: Option<toml::Value>,
    }
    toml::from_str::<FileSpec>(text)
        .map(|file| file.spec)
        .map_err(|e| {
            CliError::usage(
                format!("parsing {origin}: {e}"),
                "fix the spec key or value; memory is `mem_mb`, runtime is `max_runtime_secs`",
            )
            .into()
        })
}

/// Says the coordinator has the job and what to run next. Stdout carries it unless the
/// result owns stdout (`--wait`, or `--json` which prints the job).
fn announce(
    job: &Job,
    created: bool,
    out: &Out,
    waiting: bool,
    warning: Option<&str>,
) -> Result<()> {
    if waiting {
        // The result is coming, so the next steps would only be noise.
        eprintln!("{}", printable(&queued_line(job, created)));
        return Ok(());
    }
    if out.format() == Format::Json {
        eprintln!("{}", printable(&queued_text(job, created)));
    }
    let result = if created {
        "queued".to_string()
    } else {
        noop::already("queued")
    };
    out.emit_with(
        job,
        |j| queued_text(j, created),
        || Ok(views::job_added(&result, job, warning)),
        &hints::job_added(job.id),
    )
}

fn queued_line(job: &Job, created: bool) -> String {
    let note = if created {
        ""
    } else {
        " - already queued (no-op)"
    };
    format!("{}{note}", print::job_line(job))
}

fn queued_text(job: &Job, created: bool) -> String {
    let id = job.id;
    format!(
        "{}\nnext: `idlemax why {id}` (why it waits), `idlemax logs {id}` (its output), \
         `idlemax job cancel {id}` (stop it)",
        queued_line(job, created)
    )
}

#[cfg(test)]
mod tests;
