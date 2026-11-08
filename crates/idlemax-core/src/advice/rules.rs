//! Domain: the deterministic advisor rules. Always runs; turns cluster state, quota,
//! GitHub items and session metadata into ranked recommendations. Pure.
//!
//! This file ranks the findings; each child holds one family of rules: `jobs` (parked or
//! failed jobs), `devices` (offline and idle devices), `quota` (unused AI quota) and
//! `outside` (pull requests, agent sessions, a failed GitHub read).

mod devices;
mod jobs;
mod outside;
mod quota;

use crate::advice::inputs::AdviceInput;
use crate::types::{
    Job, Recommendation, RecommendationKind, RecommendationSource, RecommendationStatus,
};

use self::devices::{idle_device, offline_devices};
use self::jobs::problem_jobs;
use self::outside::{github_failure, unfinished_sessions, waiting_prs};
use self::quota::unused_window;

/// All rule findings, most urgent first.
pub fn evaluate(input: &AdviceInput) -> Vec<Recommendation> {
    let active: Vec<Job> = input
        .jobs
        .iter()
        .filter(|j| !j.status.is_terminal())
        .cloned()
        .collect();
    let mut out = problem_jobs(input);
    out.extend(offline_devices(input));
    out.extend(
        input
            .status
            .devices
            .iter()
            .filter_map(|d| idle_device(input, &active, d)),
    );
    out.extend(
        input
            .quota
            .iter()
            .filter_map(|q| unused_window(input, &active, q)),
    );
    out.extend(waiting_prs(input));
    out.extend(unfinished_sessions(input));
    out.extend(github_failure(input));
    out.sort_by(|a, b| {
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then(b.confidence.total_cmp(&a.confidence))
            .then(a.key.cmp(&b.key))
    });
    out
}

fn rank(kind: RecommendationKind) -> u8 {
    match kind {
        RecommendationKind::Warn => 0,
        RecommendationKind::Fix => 1,
        RecommendationKind::Assign => 2,
        RecommendationKind::Do => 3,
        RecommendationKind::FollowUp => 4,
    }
}

fn advice(
    input: &AdviceInput,
    key: String,
    kind: RecommendationKind,
    confidence: f64,
) -> Recommendation {
    Recommendation {
        at_ms: input.now_ms,
        key,
        kind,
        confidence,
        source: RecommendationSource::Rules,
        status: RecommendationStatus::Open,
        ..Default::default()
    }
}
