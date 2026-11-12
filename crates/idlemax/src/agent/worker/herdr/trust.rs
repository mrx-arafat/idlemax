//! Claude Code's folder trust dialog in a worker's fresh worktree. An interactive claude
//! started in a folder it has not seen stops there, its default answer `No, exit`, and
//! `herdr agent start` reports `agent_not_ready`. idlemax answers yes only when the dialog
//! names exactly the worktree it made for this launch; any other dialog or folder still
//! fails the start.

use std::time::Duration;

use super::{parse, strings, HerdrHost};
use crate::agent::worker::host::Launch;
use crate::agent::worker::worktree::ours;

/// How often, and how long, the agent is looked at after the dialog is answered.
const READY_WAIT: Duration = Duration::from_secs(1);
const READY_TRIES: u32 = 30;

/// Is `screen` Claude Code's trust dialog, naming exactly `folder` as the workspace? claude
/// wraps its own text to the pane's width, so the question is matched with whitespace
/// collapsed and the path as its lines joined back up to the next blank line.
pub fn is_our_trust_dialog(screen: &str, folder: &str) -> bool {
    let flat = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    let asks = flat.contains("Is this a project you created or one you trust?")
        && flat.contains("Yes, I trust this folder");
    let mut lines = screen
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '│'));
    let Some(first) = lines
        .by_ref()
        .find_map(|l| l.strip_prefix("Accessing workspace:"))
    else {
        return false;
    };
    let shown: String = std::iter::once(first.trim())
        .chain(lines)
        .skip_while(|l| l.is_empty())
        .take_while(|l| !l.is_empty())
        .collect();
    asks && !shown.is_empty() && shown == folder.split_whitespace().collect::<String>()
}

impl HerdrHost {
    /// Answers yes to the trust dialog in `pane` when it is for the launch's folder and that
    /// folder is this Run's worktree under the worktrees dir the agent resolved at launch
    /// (red team case 9), then waits for the agent to be ready. False, having sent nothing,
    /// when the pane shows anything else or the folder is not idlemax's own.
    pub(super) fn accept_trust(&self, pane: &str, launch: &Launch) -> bool {
        let folder = launch.folder.as_path();
        if ours(&launch.worktrees_dir, folder, launch.run).is_none() {
            return false;
        }
        let read = strings(&["pane", "read", pane, "--source", "recent-unwrapped"]);
        let read = [read, strings(&["--lines", "40"])].concat();
        let Ok(screen) = self.call(&read) else {
            return false;
        };
        if !is_our_trust_dialog(&screen, &folder.display().to_string()) {
            return false;
        }
        for key in ["Down", "Enter"] {
            if self
                .call(&strings(&["pane", "send-keys", pane, key]))
                .is_err()
            {
                return false;
            }
        }
        for _ in 0..READY_TRIES {
            let agents = self.parsed(&["agent", "list"], "agents", parse::agents);
            let row = agents
                .ok()
                .and_then(|a| a.into_iter().find(|r| r.pane == pane));
            if row.and_then(|r| r.status).as_deref() == Some("idle") {
                return true;
            }
            std::thread::sleep(READY_WAIT);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIALOG: &str = "\
────────────────────────────────────────
 Accessing workspace:

 /w/app/fix

 Quick safety check: Is this a project you created or one you trust? (Like your own code,
 a well-known open source project, or work from your team). If not, take a moment to
 review what's in this folder first.

 ❯ 1. No, exit
   2. Yes, I trust this folder

 Enter to confirm · Esc to cancel
";

    /// Captured from a real 54-column Herdr pane: claude wraps the path itself.
    const REAL: &str = r#"arafat@testing-team:~/idlemax-live/state/worktrees/demo/scratch-trust-d2$ claude --model haiku --strict-mcp-config --permission-mode bypassPermissions --session-id 0b6a4c8e-2f1d-4c55-9a1e-d2d2d2d2d2d2

──────────────────────────────────────────────────────
 Accessing workspace:

 /home/arafat/idlemax-live/state/worktrees/demo/scrat
 ch-trust-d2

 Quick safety check: Is this a project you created or
 one you trust? (Like your own code, a well-known
 open source project, or work from your team). If
 not, take a moment to review what's in this folder
 first.

 Claude Code'll be able to read, edit, and execute
 files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel"#;
    const REAL_FOLDER: &str = "/home/arafat/idlemax-live/state/worktrees/demo/scratch-trust-d2";

    #[test]
    fn only_the_trust_dialog_for_our_own_worktree_is_ours() {
        assert!(is_our_trust_dialog(REAL, REAL_FOLDER));
        assert!(!is_our_trust_dialog(
            REAL,
            "/home/arafat/idlemax-live/state/worktrees/demo/scrat"
        ));
        assert!(is_our_trust_dialog(DIALOG, "/w/app/fix"));
        assert!(!is_our_trust_dialog(DIALOG, "/w/app/fix2"));
        assert!(!is_our_trust_dialog(DIALOG, "/w/app"));
        let other = DIALOG.replace("Yes, I trust this folder", "Yes, allow this tool");
        assert!(!is_our_trust_dialog(&other, "/w/app/fix"));
        assert!(!is_our_trust_dialog("$ claude\nWelcome", "/w/app/fix"));
    }
}
