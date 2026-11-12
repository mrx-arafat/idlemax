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
    let in_any = |path: &str| scope.iter().any(|s| in_scope(path, s));
    ScopeFindings {
        off_limits: changed
            .iter()
            .filter(|p| any_under(p, off_limits))
            .cloned()
            .collect(),
        outside: changed
            .iter()
            .filter(|p| !scope.is_empty() && !in_any(p))
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
    if facts.head_off_branch {
        found.push("the worktree's HEAD is not the Run's branch at its tip".to_string());
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

/// `path` matches `pattern` the way `.gitignore` reads it, or lies in a folder that does: a
/// pattern without `/` (a trailing one aside) matches a name at any depth, one with `/` is
/// anchored at the repository root, `*` and `?` stay within one name and `**` spans folders.
pub fn under(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    let anchored = pattern.contains('/');
    let pattern = pattern.trim_start_matches('/');
    if pattern.is_empty() {
        return false;
    }
    let parts: Vec<&str> = path.split('/').collect();
    match anchored {
        true => from_root(&pattern.split('/').collect::<Vec<_>>(), &parts),
        false => parts.iter().any(|p| glob(pattern.as_bytes(), p.as_bytes())),
    }
}

/// The pattern's names match the path's first names, so the path is it or lies under it.
fn from_root(pattern: &[&str], parts: &[&str]) -> bool {
    match pattern.split_first() {
        None => true,
        Some((&"**", rest)) => (0..=parts.len()).any(|i| from_root(rest, &parts[i..])),
        Some((p, rest)) => parts.split_first().is_some_and(|(name, tail)| {
            glob(p.as_bytes(), name.as_bytes()) && from_root(rest, tail)
        }),
    }
}

/// `path` lies in the scope `pattern`, read as the orchestrator means it: a pattern with `/`
/// is anchored at the root, a plain name is a top-level file or folder (`tests` is not
/// `vendor/lib/tests`), and a glob without `/` matches the file name at any depth (`*.py`
/// is `calc/__init__.py`, never the folder of `x.py/evil.rs`). Off-limits and core paths
/// keep the wider `under`.
pub fn in_scope(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    if pattern.contains('/') || pattern.is_empty() {
        return under(path, pattern);
    }
    let parts: Vec<&str> = path.split('/').collect();
    match pattern.contains(['*', '?']) {
        true => parts
            .last()
            .is_some_and(|name| glob(pattern.as_bytes(), name.as_bytes())),
        false => from_root(&[pattern], &parts),
    }
}

/// One name against one glob with `*` and `?`: the iterative wildcard match, linear in the
/// name for each star, so no pattern can make it backtrack without end.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(&c) if c == b'?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((sp, sn)) => {
                    p = sp + 1;
                    n = sn + 1;
                    star = Some((sp, sn + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

fn join(items: Vec<String>) -> String {
    items.join("; ")
}

#[cfg(test)]
mod tests;
