//! Domain: idlemax's own check of a finished task (D8): whether the diff stayed in its paths,
//! whether the risk is high enough for an independent review, and the one verdict the facts
//! give. A worker saying "done" counts for nothing. Pure.

use serde::Deserialize;

use super::report::{CheckFacts, DiffStat, ReviewAnswer};
use super::run::Verdict;
use super::WorkerSpec;

/// The JSON a reviewer answers with.
pub const REVIEW_SCHEMA: &str = r#"{"type":"object","properties":{"verdict":{"type":"string","enum":["approve","concern"]},"concern":{"type":"string"}},"required":["verdict"],"additionalProperties":false}"#;
/// A diff above this many bytes is not sent to a reviewer and counts as not reviewed.
pub const MAX_REVIEW_DIFF_BYTES: usize = 200 * 1024;

/// What the scope check found in the changed paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeFindings {
    pub off_limits: Vec<String>,
    pub outside: Vec<String>,
    /// The task named no files in scope.
    pub no_scope: bool,
}

/// Each changed path under an off-limits path, each outside the scope paths, and whether the
/// task named no scope at all (D8, A4).
pub fn scope_findings(
    changed: &[String],
    scope: &[String],
    off_limits: &[String],
) -> ScopeFindings {
    let any_under = |path: &str, roots: &[String]| roots.iter().any(|r| under(path, r));
    ScopeFindings {
        off_limits: changed
            .iter()
            .filter(|p| any_under(p, off_limits))
            .cloned()
            .collect(),
        outside: changed
            .iter()
            .filter(|p| !scope.is_empty() && !any_under(p, scope))
            .cloned()
            .collect(),
        no_scope: scope.is_empty(),
    }
}

/// True when the diff is large, touches core code, or the checks cannot cover it (D8).
pub fn risk_is_high(
    diff: &DiffStat,
    review_diff_lines: u64,
    core_paths: &[String],
    has_check: bool,
    baseline_before: Option<i32>,
) -> bool {
    diff.lines() > review_diff_lines
        || diff
            .files
            .iter()
            .any(|f| core_paths.iter().any(|c| under(&f.path, c)))
        || !has_check
        || baseline_before.is_none()
}

/// The verdict of one finished task, its concern and a one-line summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub verdict: Verdict,
    pub concern: Option<String>,
    pub summary: String,
}

/// The worst finding wins: `rejected`, then `needs_you`, else `clean` (D8). New untracked
/// files never change the verdict; the concern or the summary names them.
pub fn verdict(facts: &CheckFacts, spec: &WorkerSpec) -> Judged {
    let mut changed: Vec<String> = facts.diff.files.iter().map(|f| f.path.clone()).collect();
    changed.extend(facts.modified.iter().cloned());
    let scope = scope_findings(&changed, &spec.scope_paths, &spec.off_limits);
    let rejected = rejections(facts, &scope);
    let concerns = concerns(facts, spec, &scope);
    let untracked = (!facts.untracked.is_empty())
        .then(|| format!("new untracked files: {}", facts.untracked.join(", ")));
    let summary = facts.summary.clone().unwrap_or_else(|| "no commit".into());
    let (verdict, mut found) = match (rejected.is_empty(), concerns.is_empty()) {
        (false, _) => (Verdict::Rejected, rejected),
        (true, false) => (Verdict::NeedsYou, concerns),
        (true, true) => (Verdict::Clean, Vec::new()),
    };
    if verdict == Verdict::Clean {
        let summary = [Some(summary), untracked].into_iter().flatten().collect();
        return Judged {
            verdict,
            concern: None,
            summary: join(summary),
        };
    }
    found.extend(untracked);
    Judged {
        verdict,
        concern: Some(join(found)),
        summary,
    }
}

/// The findings that reject a task: no commit, an off-limits path, a check it broke.
fn rejections(facts: &CheckFacts, scope: &ScopeFindings) -> Vec<String> {
    let mut found = Vec::new();
    if facts.commits == 0 {
        found.push("no commit was made".to_string());
    }
    if !scope.off_limits.is_empty() {
        let paths = scope.off_limits.join(", ");
        found.push(format!("changed off-limits paths: {paths}"));
    }
    if let (Some(0), Some(after)) = (facts.baseline_before, facts.baseline_after) {
        if after != 0 {
            found.push(format!(
                "the check passed before the change and fails after it (exit {after})"
            ));
        }
    }
    found
}

/// The findings the owner has to look at: the check, the scope, leftovers and the review.
fn concerns(facts: &CheckFacts, spec: &WorkerSpec, scope: &ScopeFindings) -> Vec<String> {
    let mut found = Vec::new();
    match (facts.baseline_before, facts.baseline_after) {
        (Some(0), None) => found.push("the check did not finish after the change".into()),
        (Some(before), Some(after)) if before != 0 && after != 0 => {
            found.push("the check failed before the change and still fails".into())
        }
        _ => {}
    }
    if scope.no_scope {
        found.push("the task named no files in scope".into());
    }
    if !scope.outside.is_empty() {
        let paths = scope.outside.join(", ");
        found.push(format!("changed paths outside the scope: {paths}"));
    }
    if !facts.modified.is_empty() {
        let paths = facts.modified.join(", ");
        found.push(format!("tracked files left uncommitted: {paths}"));
    }
    found.extend(review_concern(facts, spec));
    found
}

/// The reviewer's concern, or the lack of a review a high risk needed.
fn review_concern(facts: &CheckFacts, spec: &WorkerSpec) -> Option<String> {
    let review = facts.review.as_ref();
    if let Some(r) = review.filter(|r| r.answer == Some(ReviewAnswer::Concern)) {
        let what = r.concern.as_deref().unwrap_or("no detail given");
        return Some(format!("the reviewer raised a concern: {what}"));
    }
    let reviewed = review.is_some_and(|r| r.answer.is_some());
    let high = risk_is_high(
        &facts.diff,
        spec.review_diff_lines,
        &spec.core_paths,
        !spec.check.is_empty(),
        facts.baseline_before,
    );
    (high && !reviewed).then(|| "the change is high risk and no independent review happened".into())
}

/// What the reviewer reads on standard input.
pub fn review_prompt(spec: &WorkerSpec, diff: &str) -> String {
    format!(
        "Review this diff against its task. Answer only with JSON: {{\"verdict\": \"approve\" or \
         \"concern\", \"concern\": \"...\"}}. Raise a concern when the diff does more or less \
         than the task, touches files outside its scope, or could break existing behaviour.\n\n\
         Task: {}\nDone when: {}\nFiles in scope: {}\n\nDiff:\n{diff}",
        spec.task_title,
        spec.done_check,
        spec.scope_paths.join(", ")
    )
}

/// The reviewer's answer from its JSON, or None when it is not the expected shape.
pub fn parse_review(json: &serde_json::Value) -> Option<(ReviewAnswer, Option<String>)> {
    #[derive(Deserialize)]
    struct Answer {
        verdict: ReviewAnswer,
        concern: Option<String>,
    }
    let a: Answer = serde_json::from_value(json.clone()).ok()?;
    Some((a.verdict, a.concern.filter(|c| !c.trim().is_empty())))
}

/// `path` is `root` or lies under it.
fn under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    !root.is_empty()
        && (path == root || path.strip_prefix(root).is_some_and(|r| r.starts_with('/')))
}

fn join(items: Vec<String>) -> String {
    items.join("; ")
}

#[cfg(test)]
mod tests;
