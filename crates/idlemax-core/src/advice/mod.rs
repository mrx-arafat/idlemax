//! Domain: the advisor use cases. Merges fresh advice with stored advice, applies the
//! auto-accept allowlist, decides when the AI pass is due, and accepts or dismisses.
//! Pure: no I/O, time is a parameter. Adapters collect inputs and persist the plan.

pub mod ai;
pub mod inputs;
mod merge;
pub mod prompt;
pub mod rules;
mod text;

use std::collections::BTreeSet;

use crate::config::AdvisorConfig;
use crate::types::{
    JobId, JobSpec, Recommendation, RecommendationKind, RecommendationSource, RecommendationStatus,
    SpecError, UnixMs,
};

pub use ai::{is_ai_pass, AI_PASS_JOB_NAME};
pub use inputs::{
    AdviceInput, AdvisorInputs, GhItem, GithubSnapshot, GithubStatus, SessionMeta, SessionSignal,
    StoredInputs,
};
pub use merge::merge_inputs;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AdviceError {
    #[error("advice {id} is {status:?}, not open")]
    NotOpen {
        id: i64,
        status: RecommendationStatus,
    },
    #[error("advice {0} has no suggested job")]
    NoSuggestedJob(i64),
    #[error("suggested job is invalid: {0}")]
    InvalidJob(#[from] SpecError),
    #[error("AI output rejected: {0}")]
    AiOutput(String),
}

/// An AI pass the adapter should queue as a job.
#[derive(Debug, Clone, PartialEq)]
pub struct AiPass {
    pub account: String,
    pub prompt: String,
}

/// What a refresh changes. The adapter persists it; nothing here touches storage.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RefreshPlan {
    /// New advice (id 0). Status Accepted means auto-accepted: its job is in `queue`.
    pub insert: Vec<Recommendation>,
    /// Ids of open advice whose condition went away: mark them dismissed.
    pub close: Vec<i64>,
    /// Jobs to queue now (auto-accepted suggestions).
    pub queue: Vec<JobSpec>,
    pub ai_pass: Option<AiPass>,
    /// Queued AI passes too old to be useful: cancel them.
    pub cancel: Vec<JobId>,
}

/// Runs the rules engine and decides whether an AI pass is due.
/// `existing` is the stored advice of the last days in any status, for dedup by key.
pub fn plan_refresh(
    cfg: &AdvisorConfig,
    input: &AdviceInput,
    existing: &[Recommendation],
) -> RefreshPlan {
    if !cfg.enabled {
        return RefreshPlan::default();
    }
    let fresh = rules::evaluate(input);
    let ai_pass = ai::pass_due(cfg, input).map(|account| AiPass {
        account,
        prompt: prompt::prompt(input, &fresh),
    });
    RefreshPlan {
        ai_pass,
        ..merge(cfg, existing, fresh, RecommendationSource::Rules)
    }
}

/// Turns a finished AI pass (its structured JSON output) into advice. Open AI advice
/// the new pass did not repeat is closed.
pub fn plan_ai_ingest(
    cfg: &AdvisorConfig,
    templates: &[JobSpec],
    device_ids: &[String],
    existing: &[Recommendation],
    output: &serde_json::Value,
    now_ms: UnixMs,
) -> Result<RefreshPlan, AdviceError> {
    let fresh = ai::validate(output, templates, device_ids, now_ms)?;
    Ok(merge(cfg, existing, fresh, RecommendationSource::Ai))
}

fn merge(
    cfg: &AdvisorConfig,
    existing: &[Recommendation],
    fresh: Vec<Recommendation>,
    source: RecommendationSource,
) -> RefreshPlan {
    let fresh_keys: BTreeSet<&str> = fresh.iter().map(|r| r.key.as_str()).collect();
    let close = existing
        .iter()
        .filter(|r| r.status == RecommendationStatus::Open && r.source == source)
        .filter(|r| !fresh_keys.contains(r.key.as_str()))
        .map(|r| r.id)
        .collect();
    let mut seen: BTreeSet<String> = existing.iter().map(|r| r.key.clone()).collect();
    let mut plan = RefreshPlan {
        close,
        ..RefreshPlan::default()
    };
    for mut rec in fresh {
        if !seen.insert(rec.key.clone()) {
            continue;
        }
        // Only assignments: a Fix re-queues a parked job, which must stay a human call.
        let auto = rec.suggested_job.as_ref().filter(|j| {
            rec.kind == RecommendationKind::Assign && cfg.auto_accept.contains(&j.name)
        });
        if let Some(job) = auto.filter(|j| j.validate().is_ok()) {
            plan.queue.push(job.clone());
            rec.status = RecommendationStatus::Accepted;
        }
        plan.insert.push(rec);
    }
    plan
}

/// Accepting queues the suggested job; returns the advice as it should be stored.
pub fn accept(rec: &Recommendation) -> Result<(Recommendation, JobSpec), AdviceError> {
    ensure_open(rec)?;
    let job = rec
        .suggested_job
        .clone()
        .ok_or(AdviceError::NoSuggestedJob(rec.id))?;
    job.validate()?;
    let accepted = Recommendation {
        status: RecommendationStatus::Accepted,
        ..rec.clone()
    };
    Ok((accepted, job))
}

pub fn dismiss(rec: &Recommendation) -> Result<Recommendation, AdviceError> {
    ensure_open(rec)?;
    Ok(Recommendation {
        status: RecommendationStatus::Dismissed,
        ..rec.clone()
    })
}

fn ensure_open(rec: &Recommendation) -> Result<(), AdviceError> {
    match rec.status {
        RecommendationStatus::Open => Ok(()),
        status => Err(AdviceError::NotOpen { id: rec.id, status }),
    }
}
