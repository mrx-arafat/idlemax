//! Adapter: the advisor AI pass as a `claude -p` job (official CLI, owner's login, no
//! tools, no hooks, no MCP servers, no saved session), and extraction of its structured
//! JSON from the result envelope.

use idlemax_core::advice::ai::{quiet_claude_args, AI_PASS_CLI, AI_PASS_TOKEN_CLASS};
use idlemax_core::advice::{AiPass, AI_PASS_JOB_NAME};
use idlemax_core::types::{CostClass, JobSpec};
use serde_json::Value;

const PRIORITY: i32 = 10;
/// Node-based CLI; 512 MiB risks the per-job memory kill.
const MEM_MB: u64 = 768;
const MAX_RUNTIME_SECS: u64 = 600;

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("no claude result envelope in the output (truncated or not --output-format json)")]
    NoEnvelope,
    #[error("claude reported an error: {0}")]
    Reported(String),
    #[error("claude result has no structured output")]
    NoResult,
    #[error("claude result is not JSON: {0}")]
    NotJson(#[from] serde_json::Error),
}

/// The job that runs one AI pass (`quiet_claude_args`).
pub fn pass_job(pass: &AiPass, schema: &Value) -> JobSpec {
    let mut command = vec![AI_PASS_CLI.to_string()];
    command.extend(quiet_claude_args(&schema.to_string()));
    JobSpec {
        name: AI_PASS_JOB_NAME.into(),
        key: Some(format!("{AI_PASS_JOB_NAME}:{}", pass.account)),
        command,
        stdin: Some(pass.prompt.clone()),
        requires: vec![AI_PASS_CLI.into()],
        priority: PRIORITY,
        cost: CostClass::Ai,
        mem_mb: MEM_MB,
        max_runtime_secs: MAX_RUNTIME_SECS,
        ai_account: Some(pass.account.clone()),
        token_class: Some(AI_PASS_TOKEN_CLASS),
        max_attempts: 1,
        ..Default::default()
    }
}

/// The model's JSON from `claude -p --output-format json` stdout: the last
/// `{"type":"result"}` line, its `structured_output`, else its `result` text as JSON.
pub fn structured_output(stdout: &str) -> Result<Value, ClaudeError> {
    let is_result = |v: &Value| v.get("type").and_then(Value::as_str) == Some("result");
    let whole = serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .filter(is_result);
    let envelope = whole
        .or_else(|| {
            let lines = stdout.lines().rev();
            lines
                .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
                .find(is_result)
        })
        .ok_or(ClaudeError::NoEnvelope)?;
    if envelope.get("is_error").and_then(Value::as_bool) == Some(true) {
        let detail = envelope.get("result").or_else(|| envelope.get("subtype"));
        let msg = detail.and_then(Value::as_str).unwrap_or("unknown error");
        return Err(ClaudeError::Reported(msg.chars().take(300).collect()));
    }
    if let Some(v) = envelope.get("structured_output").filter(|v| v.is_object()) {
        return Ok(v.clone());
    }
    let text = envelope
        .get("result")
        .and_then(Value::as_str)
        .ok_or(ClaudeError::NoResult)?;
    Ok(serde_json::from_str(strip_fence(text))?)
}

fn strip_fence(text: &str) -> &str {
    let t = text.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_job_uses_stdin_and_disables_hooks_and_mcp() {
        let prompt = "private advisor prompt sentinel";
        let pass = AiPass {
            account: "personal".into(),
            prompt: prompt.into(),
        };
        let schema = serde_json::json!({"type": "object"});
        let spec = pass_job(&pass, &schema);
        assert_eq!(spec.key.as_deref(), Some("advisor-ai-pass:personal"));

        assert!(spec.command.iter().all(|arg| !arg.contains(prompt)));
        let stored = serde_json::to_value(&spec).unwrap();
        assert_eq!(stored.get("stdin").and_then(Value::as_str), Some(prompt));
        assert_eq!(
            spec.command,
            vec![
                "claude",
                "-p",
                "--settings",
                "{\"disableAllHooks\":true}",
                "--strict-mcp-config",
                "--output-format",
                "json",
                "--json-schema",
                &schema.to_string(),
                "--no-session-persistence",
                "--tools",
                "",
            ]
        );
        assert!(!spec.command.iter().any(|arg| arg == "--setting-sources"));
        assert!(!spec.command.iter().any(|arg| arg == "--bare"));
    }
}
