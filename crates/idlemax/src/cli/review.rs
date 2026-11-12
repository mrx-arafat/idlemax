//! Delivery: `idlemax review`, the owner's look at finished tasks and the choice for each:
//! keep the branch (and merge it with `--merge`), or discard branch and folder after one
//! confirmation (D16). The device's agent does the work; this waits for its result.

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Subcommand};
use idlemax_core::worker::run::{Run, RunState};
use idlemax_core::worker::RunId;
use idlemax_core::Paths;
use serde::Serialize;

use super::{job_wait, ApiClient, CliError, Out};

/// How long a keep or discard may take before the wait gives up.
const WAIT: Duration = Duration::from_secs(300);

#[derive(Args, Debug)]
pub struct ReviewArgs {
    #[command(subcommand)]
    cmd: Option<ReviewCmd>,
}

#[derive(Subcommand, Debug)]
enum ReviewCmd {
    /// Keep the task's branch and remove its worktree folder
    Keep {
        run: RunId,
        /// Also merge the branch into your current branch, when your checkout is clean and
        /// the merge has no conflict
        #[arg(long)]
        merge: bool,
    },
    /// Delete the task's branch and worktree, after one confirmation
    Discard {
        run: RunId,
        /// Do not ask; needed when no terminal is attached
        #[arg(long)]
        yes: bool,
    },
}

/// One finished task as `idlemax review` lists it.
#[derive(Debug, Serialize)]
struct Row {
    run: RunId,
    project: String,
    task: String,
    verdict: String,
    concern: Option<String>,
    summary: Option<String>,
    files: usize,
    lines: u64,
    branch: Option<String>,
}

pub async fn run(args: ReviewArgs, c: &ApiClient, paths: &Paths, out: &Out) -> Result<()> {
    match args.cmd {
        None => list(c, out).await,
        Some(ReviewCmd::Keep { run, merge }) => {
            let job = c.keep_run(run, merge).await?;
            job_wait::run(c, paths, job, Some(WAIT), out).await
        }
        Some(ReviewCmd::Discard { run, yes }) => {
            if !yes {
                confirm(c, run).await?;
            }
            let job = c.discard_run(run).await?;
            job_wait::run(c, paths, job, Some(WAIT), out).await
        }
    }
}

async fn list(c: &ApiClient, out: &Out) -> Result<()> {
    let runs = c.runs().await?;
    let rows: Vec<Row> = runs.iter().filter(|r| finished(r)).map(row).collect();
    let count = |state| runs.iter().filter(|r| r.state == state).count();
    let (stopped, shed) = (count(RunState::Stopped), count(RunState::Shed));
    let hints = vec![
        "Run `idlemax review keep <run>` to keep a branch, with `--merge` to merge it".to_string(),
        "Run `idlemax review discard <run>` to delete a branch and its worktree".to_string(),
    ];
    out.emit(&rows, |rows| table(rows, stopped, shed), &hints)
}

fn finished(r: &Run) -> bool {
    matches!(
        r.state,
        RunState::Clean | RunState::NeedsYou | RunState::Rejected
    )
}

fn row(r: &Run) -> Row {
    let diff = r.facts.check.as_ref().map(|f| &f.diff);
    Row {
        run: r.id,
        project: r.project.clone(),
        task: r.task_title.clone(),
        verdict: r.state.as_str().to_string(),
        concern: r.concern.clone(),
        summary: r.summary.clone(),
        files: diff.map_or(0, |d| d.files.len()),
        lines: diff.map_or(0, |d| d.lines()),
        branch: r.branch.clone(),
    }
}

fn table(rows: &[Row], stopped: usize, shed: usize) -> String {
    let mut out = Vec::new();
    if rows.is_empty() {
        out.push("no finished task waits for your review".to_string());
    }
    let mut projects: Vec<&str> = rows.iter().map(|r| r.project.as_str()).collect();
    projects.dedup();
    for project in projects {
        out.push(format!("{project}:"));
        for r in rows.iter().filter(|r| r.project == project) {
            out.push(format!(
                "  run {} {}: {} ({} files, {} lines) {}",
                r.run,
                r.verdict,
                r.summary.as_deref().unwrap_or(&r.task),
                r.files,
                r.lines,
                r.branch.as_deref().unwrap_or("no branch")
            ));
            if let Some(concern) = &r.concern {
                out.push(format!("    {concern}"));
            }
        }
    }
    if stopped + shed > 0 {
        out.push(format!(
            "{stopped} workers are stopped (idlemax start) and {shed} wait to restart"
        ));
    }
    out.join("\n")
}

/// Asks once, naming the branch and the diff size; without a terminal `--yes` is needed.
async fn confirm(c: &ApiClient, id: RunId) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "discard deletes a branch and its worktree; with no terminal to confirm it, pass --yes",
            format!("idlemax review discard {id} --yes"),
        )
        .into());
    }
    let run = c.run(id).await?;
    let r = row(&run);
    eprint!(
        "discard {} ({} files, {} lines changed) and its worktree? [y/N] ",
        r.branch.as_deref().unwrap_or("the branch"),
        r.files,
        r.lines
    );
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    match answer.trim() {
        "y" | "Y" | "yes" => Ok(()),
        _ => Err(CliError::runtime("nothing was discarded", "idlemax review").into()),
    }
}
