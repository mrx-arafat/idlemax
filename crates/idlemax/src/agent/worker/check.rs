//! Adapter: gathers the facts of a finished worker's worktree for the verdict of D8, on a
//! thread of its own: the commits since the base, what is left uncommitted or untracked, the
//! diff per file, the last commit's subject, the project's check before and after, and on a
//! high-risk diff the answer of a reviewer from another provider. idlemax runs only the
//! project's own check; the task's done check is never executed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use idlemax_core::types::{JobId, QuotaSnapshot};
use idlemax_core::worker::checks::{
    parse_review, review_prompt, risk_is_high, MAX_REVIEW_DIFF_BYTES, REVIEW_SCHEMA,
};
use idlemax_core::worker::launch::reviewer_args;
use idlemax_core::worker::report::{CheckFacts, DiffStat, FileDiff, ReviewFacts};
use idlemax_core::worker::{Reviewer, WorkerCli, WorkerSpec};

use super::cmd::{Cmd, Commands};
use super::steps::{run_project, Fence};
use super::worktree;

const GIT_LIMIT: Duration = Duration::from_secs(10);
const REVIEW_LIMIT: Duration = Duration::from_secs(300);

pub struct CheckCtx {
    pub commands: Arc<dyn Commands>,
    pub worker: WorkerSpec,
    pub job: JobId,
    pub tag: u64,
    pub folder: PathBuf,
    pub base_commit: String,
    pub remotes: Vec<String>,
    /// The agent's newest quota readings, so a review counts for the account that paid.
    pub readings: Arc<Mutex<Vec<QuotaSnapshot>>>,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(ctx: CheckCtx) -> Receiver<CheckFacts> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(gather(&ctx));
    });
    rx
}

fn gather(ctx: &CheckCtx) -> CheckFacts {
    let range = format!("{}..HEAD", ctx.base_commit);
    let commits = git(ctx, &["rev-list", "--count", &range])
        .and_then(|out| out.trim().parse().ok())
        .unwrap_or(0);
    let (modified, untracked) =
        porcelain(&git(ctx, &["status", "--porcelain"]).unwrap_or_default());
    let diff = numstat(&git(ctx, &["diff", "--numstat", &range]).unwrap_or_default());
    let summary = (commits > 0)
        .then(|| git(ctx, &["log", "-1", "--format=%s"]))
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let baseline_before = worktree::marker_of(&ctx.folder).and_then(|m| m.baseline);
    let baseline_after = match ctx.worker.check.is_empty() || cancelled(ctx) {
        true => None,
        false => check_after(ctx),
    };
    let mut facts = CheckFacts {
        commits,
        modified,
        untracked,
        diff,
        summary,
        baseline_before,
        baseline_after,
        review: None,
    };
    let w = &ctx.worker;
    let high = risk_is_high(
        &facts.diff,
        w.review_diff_lines,
        &w.core_paths,
        !w.check.is_empty(),
        baseline_before,
    );
    if let (true, Some(reviewer), false) = (high, &w.reviewer, cancelled(ctx)) {
        facts.review = Some(review(ctx, reviewer, &range));
    }
    facts
}

fn check_after(ctx: &CheckCtx) -> Option<i32> {
    let fence = Fence {
        job: ctx.job,
        tag: ctx.tag,
        remotes: &ctx.remotes,
    };
    let secs = ctx.worker.check_secs;
    run_project(
        ctx.commands.as_ref(),
        &fence,
        &ctx.folder,
        &ctx.worker.check,
        secs,
    )
    .ok()?
}

/// One review by a different provider; a diff too large to send counts as not reviewed.
fn review(ctx: &CheckCtx, r: &Reviewer, range: &str) -> ReviewFacts {
    let mut facts = ReviewFacts {
        cli: r.cli,
        account: r.account.clone(),
        model: r.model.clone(),
        quota_before: readings_of(ctx, &r.account),
        ..Default::default()
    };
    let diff = git(ctx, &["diff", range]).unwrap_or_default();
    if diff.len() > MAX_REVIEW_DIFF_BYTES {
        facts.concern = Some("the diff is too large to send to a reviewer".into());
        return facts;
    }
    if let Some((answer, concern)) = ask(ctx, r, &diff) {
        facts.answer = Some(answer);
        facts.concern = concern;
    }
    facts.quota_after = readings_of(ctx, &r.account);
    facts
}

fn ask(
    ctx: &CheckCtx,
    r: &Reviewer,
    diff: &str,
) -> Option<(idlemax_core::worker::report::ReviewAnswer, Option<String>)> {
    let schema_file = side_file(&ctx.folder, "review-schema.json");
    let out_file = side_file(&ctx.folder, "review-answer.json");
    std::fs::write(&schema_file, REVIEW_SCHEMA).ok()?;
    let (schema, out) = (
        schema_file.display().to_string(),
        out_file.display().to_string(),
    );
    let argv = reviewer_args(r.cli, &r.model, &schema, &out).ok()?;
    let (program, args) = argv.split_first()?;
    let cmd = Cmd {
        program: program.clone(),
        args: args.to_vec(),
        cwd: Some(ctx.folder.clone()),
        env: Vec::new(),
        stdin: Some(review_prompt(&ctx.worker, diff)),
        limit: REVIEW_LIMIT,
        throttle: false,
        tag: None,
    };
    let output = ctx.commands.run(&cmd).ok().filter(|o| o.code == Some(0))?;
    let json = match r.cli {
        WorkerCli::Codex => serde_json::from_str(&std::fs::read_to_string(&out_file).ok()?).ok()?,
        _ => crate::advisor::claude::structured_output(&output.stdout).ok()?,
    };
    let _ = std::fs::remove_file(&out_file);
    parse_review(&json)
}

/// The newest readings of `account`, per window.
fn readings_of(ctx: &CheckCtx, account: &str) -> Vec<QuotaSnapshot> {
    let all = ctx.readings.lock().unwrap_or_else(PoisonError::into_inner);
    all.iter()
        .filter(|s| s.account == account)
        .cloned()
        .collect()
}

/// Modified tracked paths and new untracked paths from `git status --porcelain`.
fn porcelain(text: &str) -> (Vec<String>, Vec<String>) {
    let (mut modified, mut untracked) = (Vec::new(), Vec::new());
    for line in text.lines().filter(|l| l.len() > 3) {
        let (status, path) = line.split_at(3);
        let path = path.rsplit(" -> ").next().unwrap_or(path).to_string();
        match status.trim() {
            "??" => untracked.push(path),
            _ => modified.push(path),
        }
    }
    (modified, untracked)
}

/// `git diff --numstat` lines; a binary file (`-`) counts no lines.
fn numstat(text: &str) -> DiffStat {
    let files = text.lines().filter_map(|line| {
        let mut parts = line.splitn(3, '\t');
        let added = parts.next()?.parse().unwrap_or(0);
        let removed = parts.next()?.parse().unwrap_or(0);
        Some(FileDiff {
            path: parts.next()?.to_string(),
            added,
            removed,
        })
    });
    DiffStat {
        files: files.collect(),
    }
}

fn git(ctx: &CheckCtx, args: &[&str]) -> Option<String> {
    let mut all = vec!["-C".to_string(), ctx.folder.display().to_string()];
    all.extend(args.iter().map(|s| s.to_string()));
    let cmd = Cmd {
        program: "git".into(),
        args: all,
        cwd: None,
        env: Vec::new(),
        stdin: None,
        limit: GIT_LIMIT,
        throttle: false,
        tag: None,
    };
    let out = ctx.commands.run(&cmd).ok()?;
    (out.code == Some(0)).then_some(out.stdout)
}

fn cancelled(ctx: &CheckCtx) -> bool {
    ctx.cancel.load(Ordering::SeqCst)
}

/// `<folder>.<name>`, beside the worktree and outside the repository.
fn side_file(folder: &Path, name: &str) -> PathBuf {
    PathBuf::from(format!("{}.{name}", folder.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_and_numstat_are_read() {
        let (modified, untracked) = porcelain(" M src/a.rs\nR  old.rs -> new.rs\n?? notes.txt\n");
        assert_eq!(modified, ["src/a.rs", "new.rs"]);
        assert_eq!(untracked, ["notes.txt"]);
        let diff = numstat("3\t1\tsrc/a.rs\n-\t-\tlogo.png\n");
        assert_eq!(diff.lines(), 4);
        assert_eq!(diff.files[1].path, "logo.png");
    }
}
