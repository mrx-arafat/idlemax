//! Domain: rule proposals (D18.4). After a rejected or discarded task the orchestrator may
//! propose one line for `rules.md`; this decides which proposals are kept, and the text of
//! the file once the owner approves one. Nothing reaches `rules.md` without that approval.
//! Pure.

use serde::{Deserialize, Serialize};

use crate::types::UnixMs;
use crate::worker::run::RunState;
use crate::worker::RunId;

pub const MAX_LINE_CHARS: usize = 200;
pub const MAX_OPEN: usize = 20;
/// Finished runs offered to one pass.
pub const MAX_FINISHED: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    #[default]
    Open,
    Approved,
    Rejected,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Open => "open",
            ProposalStatus::Approved => "approved",
            ProposalStatus::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Proposal {
    pub id: i64,
    pub at_ms: UnixMs,
    pub run: RunId,
    pub line: String,
    pub reason: String,
    pub status: ProposalStatus,
    pub decided_ms: Option<UnixMs>,
}

/// The `rule_proposal` of a pass's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleProposal {
    pub run: RunId,
    pub line: String,
    pub reason: String,
}

/// A rejected or discarded run that has no proposal yet, as a pass sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedRun {
    pub run: RunId,
    pub project: String,
    pub title: String,
    pub state: RunState,
    pub concern: Option<String>,
}

/// Whether a proposal is kept: `state` is the run's state (None for an unknown run),
/// `rules` the text of `rules.md` and `earlier` every stored proposal. Err says why not.
pub fn admit(
    p: &RuleProposal,
    state: Option<RunState>,
    rules: &str,
    earlier: &[Proposal],
) -> Result<(), String> {
    if !matches!(state, Some(RunState::Rejected | RunState::Discarded)) {
        return Err(format!("run {} is not rejected or discarded", p.run));
    }
    if earlier.iter().any(|e| e.run == p.run) {
        return Err(format!("run {} already has a proposal", p.run));
    }
    let chars = p.line.chars().count();
    if chars == 0 || chars > MAX_LINE_CHARS || p.line.chars().any(char::is_control) {
        return Err(format!(
            "the line must be one line of 1 to {MAX_LINE_CHARS} characters"
        ));
    }
    // A list marker or a heading would change the shape of the owner's file.
    let start = p.line.trim_start();
    if start.starts_with("- ") || start.starts_with('#') {
        return Err("the line must not start with `- ` or `#`".into());
    }
    if p.reason.chars().any(char::is_control) {
        return Err("the reason must be one line".into());
    }
    if has_rule(rules, &p.line) {
        return Err("rules.md already has the line".into());
    }
    if earlier.iter().any(|e| e.line == p.line) {
        return Err("the line was proposed before".into());
    }
    let open = earlier
        .iter()
        .filter(|e| e.status == ProposalStatus::Open)
        .count();
    if open >= MAX_OPEN {
        return Err(format!("{MAX_OPEN} proposals are already open"));
    }
    Ok(())
}

/// `rules` with `line` as its last line, written as a list item when the file's last line is
/// one; unchanged when the line is already there, so approving twice appends once.
pub fn append_rule(rules: &str, line: &str) -> String {
    if has_rule(rules, line) {
        return rules.to_string();
    }
    let listed = rules
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| l.trim_start().starts_with("- "));
    let mut text = rules.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if listed {
        text.push_str("- ");
    }
    text.push_str(line);
    text.push('\n');
    text
}

/// A line of the file is the rule, alone or as a list item.
fn has_rule(rules: &str, line: &str) -> bool {
    let line = line.trim();
    rules
        .lines()
        .map(str::trim)
        .any(|l| l == line || l.strip_prefix("- ").is_some_and(|item| item.trim() == line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(run: RunId, line: &str) -> RuleProposal {
        RuleProposal {
            run,
            line: line.into(),
            reason: "the worker deleted a fixture".into(),
        }
    }

    fn stored(run: RunId, line: &str, status: ProposalStatus) -> Proposal {
        Proposal {
            run,
            line: line.into(),
            status,
            ..Proposal::default()
        }
    }

    #[test]
    fn admit_keeps_one_new_single_line_per_finished_run() {
        let rules = "# rules\n- Never touch migrations.\n";
        let line = "Never delete a test fixture.";
        let earlier = [
            stored(1, line, ProposalStatus::Open),
            stored(2, "Keep commits small.", ProposalStatus::Rejected),
        ];
        let rejected = Some(RunState::Rejected);
        assert_eq!(
            admit(&proposal(3, line), Some(RunState::Discarded), "", &[]),
            Ok(())
        );
        let refused = [
            admit(&proposal(3, line), Some(RunState::Clean), rules, &[]),
            admit(&proposal(3, line), None, rules, &[]),
            admit(&proposal(1, "Another line."), rejected, rules, &earlier),
            admit(&proposal(3, "two\nlines"), rejected, rules, &[]),
            admit(&proposal(3, ""), rejected, rules, &[]),
            admit(&proposal(3, &"x".repeat(201)), rejected, rules, &[]),
            admit(
                &proposal(3, "Never touch migrations."),
                rejected,
                rules,
                &[],
            ),
            admit(
                &proposal(3, "Keep commits small."),
                rejected,
                rules,
                &earlier,
            ),
            admit(&proposal(3, "- Listed already."), rejected, rules, &[]),
            admit(&proposal(3, "# A heading"), rejected, rules, &[]),
            admit(
                &RuleProposal {
                    reason: "two\nlines".into(),
                    ..proposal(3, line)
                },
                rejected,
                rules,
                &[],
            ),
        ];
        for (i, r) in refused.iter().enumerate() {
            assert!(r.is_err(), "case {i}: {r:?}");
        }
        let open: Vec<Proposal> = (10..30)
            .map(|run| stored(run, &format!("line {run}"), ProposalStatus::Open))
            .collect();
        assert!(admit(&proposal(3, line), rejected, rules, &open).is_err());
    }

    #[test]
    fn an_approved_line_is_appended_once_in_the_files_style() {
        let listed = append_rule("# rules\n- One.\n", "Two.");
        assert_eq!(listed, "# rules\n- One.\n- Two.\n");
        assert_eq!(append_rule(&listed, "Two."), listed);
        assert_eq!(append_rule("Plain text", "Two."), "Plain text\nTwo.\n");
        assert_eq!(append_rule("", "Two."), "Two.\n");
    }
}
