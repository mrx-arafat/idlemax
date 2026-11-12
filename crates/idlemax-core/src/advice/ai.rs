//! Domain: the advisor AI pass. Decides when it is due, defines its JSON output
//! schema, and strictly validates its output into recommendations. Suggested jobs can
//! only name owner templates, never carry a command from the model. Pure.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::advice::inputs::AdviceInput;
use crate::advice::text::{self, HOUR_MS};
use crate::advice::AdviceError;
use crate::config::AdvisorConfig;
use crate::scheduler::{spendable_headroom, token_class_pct};
use crate::types::{
    Job, JobId, JobSpec, JobStatus, Recommendation, RecommendationKind, RecommendationSource,
    RecommendationStatus, TokenClass, UnixMs,
};

pub const AI_PASS_JOB_NAME: &str = "advisor-ai-pass";
pub const AI_PASS_TOKEN_CLASS: TokenClass = TokenClass::Medium;
/// CLI the pass runs through; the device must have it on PATH.
pub const AI_PASS_CLI: &str = "claude";
const AI_PASS_INTERVAL_MS: i64 = 6 * HOUR_MS;
const MAX_ITEMS: usize = 8;
const MAX_TITLE: usize = 120;
const MAX_WHY: usize = 600;
const MAX_EVIDENCE: usize = 6;
const MAX_EVIDENCE_LEN: usize = 200;

/// The arguments of a one-shot `claude -p` run that answers JSON to `schema`, with no hooks,
/// no MCP servers, no saved session and no tools: the advisor pass and a worker's reviewer.
/// `--no-session-persistence` keeps the run out of the session files that interactive-use
/// detection reads.
pub fn quiet_claude_args(schema: &str) -> Vec<String> {
    [
        "-p",
        "--settings",
        "{\"disableAllHooks\":true}",
        "--strict-mcp-config",
        "--output-format",
        "json",
        "--json-schema",
        schema,
        "--no-session-persistence",
        "--tools",
        "",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub fn is_ai_pass(spec: &JobSpec) -> bool {
    spec.name == AI_PASS_JOB_NAME
}

/// The account to spend when a pass is due: configured, spendable for a Medium job,
/// none in flight, none in the last 6 h, and an online device able to run it.
pub fn pass_due(cfg: &AdvisorConfig, input: &AdviceInput) -> Option<String> {
    let account = cfg.ai_account.as_deref().filter(|_| cfg.enabled)?;
    if spendable_headroom(&input.quota, account) < token_class_pct(Some(AI_PASS_TOKEN_CLASS)) {
        return None;
    }
    let recent = input.jobs.iter().filter(|j| is_ai_pass(&j.spec)).any(|j| {
        let young = input.now_ms - j.created_ms < AI_PASS_INTERVAL_MS;
        j.status.is_active() || young
    });
    let runnable = input.status.devices.iter().any(|d| {
        d.online
            && d.info.ai_accounts.iter().any(|a| a == account)
            && d.info.capabilities.iter().any(|c| c == AI_PASS_CLI)
    });
    (!recent && runnable).then(|| account.to_string())
}

/// Queued AI passes older than the interval: their prompt is stale, so they are cancelled
/// and no longer block a fresh pass.
pub fn stale_passes(input: &AdviceInput) -> Vec<JobId> {
    let stale = |j: &&Job| {
        is_ai_pass(&j.spec)
            && j.status == JobStatus::Queued
            && input.now_ms - j.created_ms >= AI_PASS_INTERVAL_MS
    };
    input.jobs.iter().filter(stale).map(|j| j.id).collect()
}

/// JSON Schema the CLI enforces on the model's structured output.
pub fn schema() -> Value {
    let nullable = json!({ "type": ["string", "null"] });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["recommendations"],
        "properties": { "recommendations": {
            "type": "array", "maxItems": MAX_ITEMS,
            "items": {
                "type": "object", "additionalProperties": false,
                "required": ["kind", "title", "why", "evidence", "confidence", "template", "device"],
                "properties": {
                    "kind": { "type": "string", "enum": ["assign", "do", "fix", "follow_up", "warn"] },
                    "title": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE },
                    "why": { "type": "string", "maxLength": MAX_WHY },
                    "evidence": { "type": "array", "maxItems": MAX_EVIDENCE,
                        "items": { "type": "string", "maxLength": MAX_EVIDENCE_LEN } },
                    "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
                    "template": nullable, "device": nullable,
                }
            }
        }}
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    recommendations: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    kind: RecommendationKind,
    title: String,
    why: String,
    evidence: Vec<String>,
    confidence: f64,
    template: Option<String>,
    device: Option<String>,
}

/// Strict check of the model's output. Any schema violation rejects the whole pass;
/// an unknown template or device only drops the suggested job and says so in evidence.
pub fn validate(
    output: &Value,
    templates: &[JobSpec],
    device_ids: &[String],
    now_ms: UnixMs,
) -> Result<Vec<Recommendation>, AdviceError> {
    let out = Output::deserialize(output).map_err(|e| AdviceError::AiOutput(e.to_string()))?;
    if out.recommendations.len() > MAX_ITEMS {
        return Err(AdviceError::AiOutput(format!(
            "more than {MAX_ITEMS} recommendations"
        )));
    }
    out.recommendations
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            check_item(&item).map_err(|e| AdviceError::AiOutput(format!("item {i}: {e}")))?;
            Ok(to_recommendation(item, templates, device_ids, now_ms))
        })
        .collect()
}

fn check_item(item: &Item) -> Result<(), String> {
    let chars = |s: &str| s.chars().count();
    if item.title.trim().is_empty() || chars(&item.title) > MAX_TITLE {
        return Err(format!("title must be 1 to {MAX_TITLE} chars"));
    }
    if chars(&item.why) > MAX_WHY {
        return Err(format!("why is over {MAX_WHY} chars"));
    }
    if item.evidence.len() > MAX_EVIDENCE
        || item.evidence.iter().any(|e| chars(e) > MAX_EVIDENCE_LEN)
    {
        return Err("evidence too long".into());
    }
    if !(0.0..=1.0).contains(&item.confidence) {
        return Err("confidence outside 0..1".into());
    }
    Ok(())
}

fn to_recommendation(
    item: Item,
    templates: &[JobSpec],
    device_ids: &[String],
    now_ms: UnixMs,
) -> Recommendation {
    let mut evidence = item.evidence;
    let mut job = item.template.as_deref().and_then(|name| {
        let found = templates
            .iter()
            .find(|t| t.name == name && !is_ai_pass(t))
            .cloned();
        if found.is_none() {
            evidence.push(format!("unknown template {} ignored", text::clip(name, 60)));
        }
        found
    });
    if let (Some(spec), Some(device)) = (job.as_mut(), item.device.as_deref()) {
        let known = device_ids.iter().any(|d| d == device);
        if known && (spec.devices.is_empty() || spec.devices.iter().any(|d| d == device)) {
            spec.devices = vec![device.to_string()];
        } else {
            evidence.push(format!(
                "device {} not allowed for this template",
                text::clip(device, 60)
            ));
        }
    }
    Recommendation {
        id: 0,
        at_ms: now_ms,
        key: format!("ai:{}", text::slug(&item.title)),
        kind: item.kind,
        title: item.title.trim().to_string(),
        why: item.why,
        evidence,
        suggested_job: job,
        confidence: item.confidence,
        source: RecommendationSource::Ai,
        status: RecommendationStatus::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_keeps_template_jobs_only_and_rejects_schema_violations() {
        let templates = vec![JobSpec {
            name: "repo-health".into(),
            command: vec!["true".into()],
            ..Default::default()
        }];
        let devices = vec!["mac".to_string()];
        let output = json!({ "recommendations": [
            { "kind": "assign", "title": "Run repo health on the Mac", "why": "idle",
              "evidence": [], "confidence": 0.8, "template": "repo-health", "device": "mac" },
            { "kind": "do", "title": "Review org/app#1", "why": "waiting", "evidence": [],
              "confidence": 0.6, "template": "rm-rf-everything", "device": null },
        ]});
        let recs = validate(&output, &templates, &devices, 5).unwrap();
        let job = recs[0]
            .suggested_job
            .as_ref()
            .map(|j| (j.name.as_str(), j.devices.clone()));
        assert_eq!(job, Some(("repo-health", vec!["mac".to_string()])));
        assert_eq!(recs[1].suggested_job, None);

        let command = json!({ "recommendations": [{ "kind": "warn", "title": "t", "why": "",
            "evidence": [], "confidence": 0.5, "template": null, "device": null, "command": ["sh"] }]});
        assert!(validate(&command, &templates, &devices, 5).is_err());
    }
}
