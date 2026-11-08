//! Rules about signals from outside the cluster: pull requests waiting on the owner, agent
//! sessions that look unfinished, and a failed GitHub read. Part of the advisor rules (domain
//! layer, pure).

use crate::advice::inputs::{AdviceInput, GhItem, GithubStatus};
use crate::advice::text::{self, DAY_MS, HOUR_MS};
use crate::types::{Recommendation, RecommendationKind};

use super::advice;

const PR_WAIT_MS: i64 = 2 * DAY_MS;

const SESSION_QUIET_MS: i64 = 2 * HOUR_MS;

const SESSION_MAX_AGE_MS: i64 = 3 * DAY_MS;

pub(super) fn waiting_prs(input: &AdviceInput) -> Vec<Recommendation> {
    let gh = &input.github;
    let review = gh
        .review_requested
        .iter()
        .filter(|i| !i.is_draft)
        .filter_map(|i| stale_pr(input, i, RecommendationKind::Do, "Review", "pr_review"));
    let assigned = gh.assigned_prs.iter().filter_map(|i| {
        stale_pr(
            input,
            i,
            RecommendationKind::FollowUp,
            "Follow up on",
            "pr_assigned",
        )
    });
    review.chain(assigned).collect()
}

fn stale_pr(
    input: &AdviceInput,
    item: &GhItem,
    kind: RecommendationKind,
    verb: &str,
    key_prefix: &str,
) -> Option<Recommendation> {
    let quiet = input.now_ms - item.updated_ms;
    if quiet < PR_WAIT_MS {
        return None;
    }
    Some(Recommendation {
        title: text::clip(&format!("{verb} {}: {}", item.reference(), item.title), 120),
        why: format!("Waiting on you; no activity for {}.", text::duration(quiet)),
        evidence: vec![
            item.url.clone(),
            format!("author {}", item.author),
            format!(
                "opened {} ago",
                text::duration(input.now_ms - item.created_ms)
            ),
        ],
        ..advice(
            input,
            format!("{key_prefix}:{}", item.reference()),
            kind,
            0.8,
        )
    })
}

pub(super) fn unfinished_sessions(input: &AdviceInput) -> Vec<Recommendation> {
    let now = input.now_ms;
    input
        .sessions
        .iter()
        .filter(|s| !s.signals.is_empty())
        .filter(|s| (SESSION_QUIET_MS..=SESSION_MAX_AGE_MS).contains(&(now - s.last_activity_ms)))
        .map(|s| {
            let name = s.title.as_deref().unwrap_or(&s.id);
            let place = s
                .project
                .as_deref()
                .map(|p| format!(" in {p}"))
                .unwrap_or_default();
            let what: Vec<&str> = s.signals.iter().map(|x| x.describe()).collect();
            let quiet = text::duration(now - s.last_activity_ms);
            let key = format!("session:{}:{}:{}", s.tool, s.id, s.last_activity_ms);
            Recommendation {
                title: text::clip(
                    &format!("Session '{name}'{place} still has {}", what.join(" and ")),
                    120,
                ),
                why: format!("Quiet for {quiet}; it looks unfinished."),
                evidence: vec![format!("{} session {}", s.tool, s.id)],
                ..advice(input, key, RecommendationKind::FollowUp, 0.5)
            }
        })
        .collect()
}

pub(super) fn github_failure(input: &AdviceInput) -> Option<Recommendation> {
    let GithubStatus::Failed(err) = &input.github.status else {
        return None;
    };
    let key = format!("github_failed:{}", text::day(input.now_ms));
    Some(Recommendation {
        title: "GitHub inputs unavailable".into(),
        why: "The advisor could not read PRs and issues, so PR advice is missing.".into(),
        evidence: vec![text::clip(err, 200)],
        ..advice(input, key, RecommendationKind::Warn, 0.3)
    })
}
