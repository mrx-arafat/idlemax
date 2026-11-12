//! Domain: the brief a worker starts with, in fixed sections (D6, D8), and the one line that
//! points the agent at it for a first start or a continued session (D11). Pure.

use super::WorkerSpec;

/// The brief of one task: goal, files in scope, what not to touch, the done check, the test
/// rule, the limits of D6, then the owner's `rules.md`.
pub fn brief(spec: &WorkerSpec, rules: &str) -> String {
    let mut out = format!(
        "# Task: {}\n\n## Goal\n\n{}\n",
        spec.task_title, spec.task_title
    );
    out.push_str("\n## Files in scope\n\n");
    if spec.scope_paths.is_empty() {
        out.push_str("- No files were named. Change nothing and say so in your last message.\n");
    }
    for path in &spec.scope_paths {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str("\n## Do not touch\n\n");
    for path in &spec.off_limits {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str("- Any path outside the files in scope.\n");
    out.push_str(&format!(
        "- The owner's own checkout at {}; work only in this worktree.\n",
        spec.repo
    ));
    out.push_str(&format!("\n## Done when\n\n{}\n", spec.done_check));
    out.push_str(
        "\n## Tests\n\nRun focused tests only, never a whole suite, and add no test the task \
         does not need.\n",
    );
    out.push_str(
        "\n## Limits\n\nCommit locally on this branch. Never push, open or merge a pull \
         request, deploy, touch a database, send a message or spend money.\n",
    );
    if !rules.trim().is_empty() {
        out.push_str(&format!("\n## Rules\n\n{}\n", rules.trim()));
    }
    out
}

/// The first message of a fresh session.
pub fn start_line(brief_path: &str) -> String {
    format!("Read the brief in {brief_path} and do the task it describes.")
}

/// The first message of a continued session.
pub fn resume_line(brief_path: &str) -> String {
    format!("Continue the task in {brief_path} from where you stopped.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brief_holds_every_fixed_section_in_order() {
        let spec = WorkerSpec {
            task_title: "Fix the flaky sync test".into(),
            done_check: "cargo test -p app sync passes".into(),
            repo: "/home/me/app".into(),
            scope_paths: vec!["src/sync.rs".into()],
            off_limits: vec!["migrations/".into()],
            ..Default::default()
        };
        let text = brief(&spec, "Never break existing behaviour.");
        let order = [
            "## Goal\n\nFix the flaky sync test",
            "## Files in scope\n\n- src/sync.rs",
            "## Do not touch\n\n- migrations/",
            "- The owner's own checkout at /home/me/app",
            "## Done when\n\ncargo test -p app sync passes",
            "focused tests only",
            "Never push, open or merge a pull request",
            "## Rules\n\nNever break existing behaviour.",
        ];
        let mut at = 0;
        for part in order {
            let found = text[at..].find(part).unwrap_or_else(|| panic!("{part}"));
            at += found;
        }
        assert!(start_line("/w.brief.md").contains("/w.brief.md"));
        assert!(resume_line("/w.brief.md").starts_with("Continue"));
    }
}
