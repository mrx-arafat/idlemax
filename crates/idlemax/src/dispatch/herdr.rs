//! Adapter: reads the owner's Herdr session (D5): the folders of its panes from
//! `herdr pane list` and which agents are working from `herdr agent list`. Read-only.
//! The parsers are pure, ignore unknown keys and skip a row without `cwd`.

use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

const HERDR_TIMEOUT: Duration = Duration::from_secs(5);

/// One agent Herdr shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrAgent {
    pub cwd: String,
    pub working: bool,
}

/// What Herdr showed on this device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HerdrView {
    /// The working folders of the panes.
    pub panes: Vec<String>,
    pub agents: Vec<HerdrAgent>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: T,
}

#[derive(Deserialize)]
struct Panes {
    #[serde(default)]
    panes: Vec<Row>,
}

#[derive(Deserialize)]
struct Agents {
    #[serde(default)]
    agents: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    cwd: Option<String>,
    agent_status: Option<String>,
}

/// The pane folders of `herdr pane list` JSON; None without `result`.
pub fn parse_panes(json: &str) -> Option<Vec<String>> {
    let e: Envelope<Panes> = serde_json::from_str(json).ok()?;
    Some(e.result.panes.into_iter().filter_map(|r| r.cwd).collect())
}

/// The agents of `herdr agent list` JSON; None without `result`.
pub fn parse_agents(json: &str) -> Option<Vec<HerdrAgent>> {
    let e: Envelope<Agents> = serde_json::from_str(json).ok()?;
    let agents = e.result.agents.into_iter().filter_map(|r| {
        Some(HerdrAgent {
            working: r.agent_status.as_deref() == Some("working"),
            cwd: r.cwd?,
        })
    });
    Some(agents.collect())
}

/// Both lists, or None when Herdr is missing, slow or answers without `result`.
pub async fn read() -> Option<HerdrView> {
    let panes = parse_panes(&run(&["pane", "list"]).await?)?;
    let agents = parse_agents(&run(&["agent", "list"]).await?)?;
    Some(HerdrView { panes, agents })
}

async fn run(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("herdr");
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(HERDR_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_lists_give_folders_and_working_agents() {
        let panes = r#"{"result":{"panes":[
            {"pane_id":"p1","cwd":"/a/app","new_key":1},
            {"pane_id":"p2","cwd":"/a/lib"},
            {"pane_id":"p3"}]}}"#;
        assert_eq!(
            parse_panes(panes),
            Some(vec!["/a/app".to_string(), "/a/lib".to_string()])
        );
        let agents = r#"{"result":{"agents":[
            {"kind":"claude","cwd":"/a/app","agent_status":"working","pane_id":"p1"},
            {"kind":"codex","cwd":"/a/lib","agent_status":"idle"}]}}"#;
        let agents = parse_agents(agents).unwrap();
        assert_eq!(
            agents,
            [
                HerdrAgent {
                    cwd: "/a/app".into(),
                    working: true
                },
                HerdrAgent {
                    cwd: "/a/lib".into(),
                    working: false
                }
            ]
        );
        assert_eq!(parse_panes(r#"{"error": "x"}"#), None);
        assert_eq!(parse_agents(r#"{"error": "x"}"#), None);
    }
}
