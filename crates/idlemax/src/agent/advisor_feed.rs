//! The agent's advisor feed (adapter): reads the GitHub summary (`gh`) and the session
//! metadata on this device and sends them to the coordinator, so a coordinator on another
//! host still advises on them. Metadata only: never message text leaves the device.

use std::path::PathBuf;
use std::time::Duration;

use idlemax_core::advice::AdvisorInputs;
use idlemax_core::types::{DeviceId, UnixMs};

use super::client::CoordClient;
use crate::advisor::{github, sessions};
use crate::state::now_ms;

/// Bounds each `gh` search, so a hung `gh` never holds the feed past about this long.
const GH_TIMEOUT: Duration = Duration::from_secs(20);

/// One feed run: collect, send, log. The next tick is the retry, so nothing retries here.
pub async fn run(client: CoordClient, device: DeviceId) {
    let inputs = collect(device, user_home(), now_ms()).await;
    match client.advisor_inputs(&inputs).await {
        Ok(()) => tracing::debug!(
            sessions = inputs.sessions.len(),
            github = ?inputs.github.status,
            "advisor inputs sent"
        ),
        Err(e) => tracing::warn!("advisor inputs not delivered: {e}"),
    }
}

/// Reads the inputs under `home`; session files are blocking reads, so they go off-thread.
async fn collect(device: DeviceId, home: Option<PathBuf>, now: UnixMs) -> AdvisorInputs {
    let github = github::read(GH_TIMEOUT).await;
    let scan = tokio::task::spawn_blocking(move || {
        home.map(|h| sessions::read(&h, now)).unwrap_or_default()
    });
    let sessions = scan.await.unwrap_or_else(|e| {
        tracing::warn!("session scan failed: {e}");
        Vec::new()
    });
    AdvisorInputs {
        device,
        github,
        sessions,
    }
}

/// Set and non-empty only: an empty HOME must never resolve to some other directory.
fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Claude transcript whose every text field carries a marker that must never leave.
    const TRANSCRIPT: &str = r#"{"type":"user","cwd":"/Users/leak-user/code/myproj","message":{"content":"LEAK-USER-PROMPT fix the login"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"LEAK-ASSISTANT-REPLY"},{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"LEAK-TODO-TEXT","status":"pending"}]}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"LEAK-TOOL-OUTPUT\ntest result: FAILED. 1 passed; 1 failed"}]}}
{"type":"ai-title","aiTitle":"Fix login"}
"#;

    #[test]
    fn payload_carries_session_metadata_and_no_message_text() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join(".claude/projects/-leak-user-code-myproj");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = home.path().join("myproj");
        std::fs::create_dir(&cwd).unwrap();
        let transcript = TRANSCRIPT.replace("/Users/leak-user/code/myproj", &cwd.to_string_lossy());
        std::fs::write(project.join("abc123.jsonl"), transcript).unwrap();

        let inputs = AdvisorInputs {
            device: "mac".into(),
            sessions: sessions::read(home.path(), now_ms()),
            ..AdvisorInputs::default()
        };
        let wire = serde_json::to_string(&inputs).unwrap();

        for kept in [
            "Fix login",
            "myproj",
            "abc123",
            "tests_failing",
            "todos_open",
        ] {
            assert!(wire.contains(kept), "payload lost {kept}: {wire}");
        }
        for leaked in ["LEAK-", "leak-user", "fix the login"] {
            assert!(!wire.contains(leaked), "payload leaked {leaked}: {wire}");
        }
    }
}
