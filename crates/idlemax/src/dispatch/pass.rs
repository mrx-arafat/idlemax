//! Adapter: the orchestrator pass as a `claude -p --model opus` job: the owner's login, no
//! tools, no hooks, no MCP servers and no saved session, the prompt on stdin, admitted by the
//! worker lines (A16). Its answer is read with the advisor's envelope reader.

use idlemax_core::advice::ai::AI_PASS_TOKEN_CLASS;
use idlemax_core::dispatch::orchestrator::{DuePass, PASS_JOB_NAME, PASS_MODEL};
use idlemax_core::types::{Admission, CostClass, JobSpec};
use serde_json::Value;

const CLI: &str = "claude";
/// Node-based CLI; 512 MiB risks the per-job memory kill.
const MEM_MB: u64 = 768;
const MAX_RUNTIME_SECS: u64 = 600;

/// The job that runs one orchestrator pass.
pub fn pass_job(due: &DuePass, prompt: String, schema: &Value) -> JobSpec {
    let command = [
        CLI,
        "-p",
        "--model",
        PASS_MODEL,
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
    ];
    JobSpec {
        name: PASS_JOB_NAME.into(),
        key: Some(PASS_JOB_NAME.into()),
        command: command.iter().map(|s| s.to_string()).collect(),
        stdin: Some(prompt),
        requires: vec![CLI.into()],
        devices: vec![due.device.clone()],
        cost: CostClass::Ai,
        mem_mb: MEM_MB,
        max_runtime_secs: MAX_RUNTIME_SECS,
        ai_account: Some(due.account.clone()),
        token_class: Some(AI_PASS_TOKEN_CLASS),
        max_attempts: 1,
        admission: Admission::Lines,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::dispatch::orchestrator::{is_pass, schema};

    use super::*;

    #[test]
    fn the_pass_runs_opus_with_no_tools_and_the_prompt_on_stdin() {
        let due = DuePass {
            account: "max".into(),
            device: "mac".into(),
        };
        let spec = pass_job(&due, "SECRET PROMPT".into(), &schema());
        let argv = &spec.command;
        let after = |flag: &str| {
            argv.iter()
                .position(|a| a == flag)
                .map(|i| argv[i + 1].as_str())
        };
        assert_eq!(after("--model"), Some("opus"));
        assert_eq!(after("--tools"), Some(""));
        assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
        assert!(argv.iter().any(|a| a == "--json-schema"));
        assert!(!argv.iter().any(|a| a.contains("SECRET")));
        assert_eq!(spec.stdin.as_deref(), Some("SECRET PROMPT"));
        assert_eq!(spec.admission, Admission::Lines);
        assert!(is_pass(&spec));
        assert_eq!(spec.validate(), Ok(()));
    }
}
