//! Domain: the `.claude.json` of the sandbox's Claude home (A13, Spec 6 Task 2): only the keys
//! the CLI needs to run from the owner's file, never the owner's MCP servers or projects, and
//! the worker's worktree marked trusted so no trust dialog appears. Pure.

use serde_json::{Map, Value};

/// The keys of the owner's `.claude.json` a sandboxed CLI gets.
const KEPT: [&str; 6] = [
    "oauthAccount",
    "userID",
    "hasCompletedOnboarding",
    "lastOnboardingVersion",
    "theme",
    "bypassPermissionsModeAccepted",
];

/// The sandbox's file: the kept keys of `owner`, the sandbox's own `projects` from `existing`,
/// and `worktree` trusted.
pub fn sandbox_claude_json(owner: &Value, existing: Option<&Value>, worktree: &str) -> Value {
    let mut out = Map::new();
    for key in KEPT {
        if let Some(v) = owner.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    let mut projects = existing
        .and_then(|e| e.get("projects"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let entry = projects
        .entry(worktree.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    }
    out.insert("projects".into(), Value::Object(projects));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_owners_servers_and_projects_stay_out_and_the_worktree_is_trusted() {
        let owner = json!({
            "oauthAccount": {"emailAddress": "o@x"},
            "userID": "u1",
            "mcpServers": {"db": {"command": "psql", "env": {"PGPASSWORD": "s"}}},
            "projects": {"/home/o/app": {"hasTrustDialogAccepted": true}},
        });
        let existing =
            json!({"projects": {"/s/worktrees/app/t0": {"hasTrustDialogAccepted": true}}});
        let out = sandbox_claude_json(&owner, Some(&existing), "/s/worktrees/app/t1");
        assert_eq!(out["oauthAccount"]["emailAddress"], "o@x");
        assert_eq!(out["userID"], "u1");
        assert!(out.get("mcpServers").is_none());
        assert!(out["projects"].get("/home/o/app").is_none());
        assert_eq!(
            out["projects"]["/s/worktrees/app/t0"]["hasTrustDialogAccepted"],
            true
        );
        assert_eq!(
            out["projects"]["/s/worktrees/app/t1"]["hasTrustDialogAccepted"],
            true
        );
    }
}
