//! Domain: the state label and one-line summary of a Run (D17), the one text every surface
//! shows for a worker. Pure.

use serde::{Deserialize, Serialize};

use crate::worker::run::{Run, RunState};

/// The longest summary, in characters.
pub const SUMMARY_MAX_CHARS: usize = 160;

/// A Run as the surfaces see it: `frozen` is true when the Run's live job is `paused` (A24).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunLine {
    pub run: Run,
    pub frozen: bool,
}

pub fn run_label(state: RunState, frozen: bool) -> &'static str {
    if frozen && is_live(state) {
        return "paused";
    }
    match state {
        RunState::Starting => "starting",
        RunState::Running => "working",
        RunState::Checking => "checking",
        RunState::Clean => "ready for review",
        RunState::NeedsYou => "needs you",
        RunState::Rejected => "rejected",
        RunState::Stopped => "stopped by you",
        RunState::Shed => "back in queue",
        RunState::Kept => "kept",
        RunState::Discarded => "discarded",
    }
}

/// One line of at most `SUMMARY_MAX_CHARS` on what the Run is doing or how it ended.
pub fn run_summary(line: &RunLine) -> String {
    let run = &line.run;
    let task = &run.task_title;
    let concern = run.concern.as_deref().unwrap_or("no reason given");
    let text = if line.frozen && is_live(run.state) {
        format!("{task}: frozen by a pause, continues on resume")
    } else {
        match run.state {
            RunState::Starting => format!("setting up {task}"),
            RunState::Running => {
                let provider = run.provider.cli().unwrap_or("api");
                format!("{task} on {provider} {}", run.model)
            }
            RunState::Checking => format!("idlemax is checking {task}"),
            RunState::Clean => format!("{task}: checks passed"),
            RunState::NeedsYou => format!("{task}: {concern}"),
            RunState::Rejected => format!("{task}: {concern}, branch kept"),
            RunState::Stopped => {
                format!("{task}: state saved, continues after you start it again")
            }
            RunState::Shed => format!("{task}: state saved, restarts when memory and quota allow"),
            RunState::Kept => format!("{task}: accepted"),
            RunState::Discarded => format!("{task}: branch and worktree removed"),
        }
    };
    one_line(&text)
}

/// A pause freezes these in place; the others have no live job to freeze.
fn is_live(state: RunState) -> bool {
    matches!(
        state,
        RunState::Starting | RunState::Running | RunState::Checking
    )
}

/// `text` on one line of at most `SUMMARY_MAX_CHARS`.
pub fn one_line(text: &str) -> String {
    plain(text).chars().take(SUMMARY_MAX_CHARS).collect()
}

/// `text` with every control character and every line or paragraph separator (any
/// whitespace but the space) made a space, so a terminal draws it as one line and no escape
/// sequence reaches it.
pub fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || (c.is_whitespace() && c != ' ') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(state: RunState, concern: Option<&str>, frozen: bool) -> RunLine {
        RunLine {
            run: Run {
                task_title: "fix the login test".into(),
                provider: crate::quota::Provider::Claude,
                model: "sonnet".into(),
                state,
                concern: concern.map(str::to_string),
                ..Run::default()
            },
            frozen,
        }
    }

    #[test]
    fn each_state_has_the_label_and_summary_of_the_table() {
        let t = "fix the login test";
        let table = [
            (RunState::Starting, "starting", format!("setting up {t}")),
            (
                RunState::Running,
                "working",
                format!("{t} on claude sonnet"),
            ),
            (
                RunState::Checking,
                "checking",
                format!("idlemax is checking {t}"),
            ),
            (
                RunState::Clean,
                "ready for review",
                format!("{t}: checks passed"),
            ),
            (RunState::NeedsYou, "needs you", format!("{t}: tests fail")),
            (
                RunState::Rejected,
                "rejected",
                format!("{t}: tests fail, branch kept"),
            ),
            (
                RunState::Stopped,
                "stopped by you",
                format!("{t}: state saved, continues after you start it again"),
            ),
            (
                RunState::Shed,
                "back in queue",
                format!("{t}: state saved, restarts when memory and quota allow"),
            ),
            (RunState::Kept, "kept", format!("{t}: accepted")),
            (
                RunState::Discarded,
                "discarded",
                format!("{t}: branch and worktree removed"),
            ),
        ];
        assert_eq!(table.len(), RunState::ALL.len());
        for (state, label, summary) in table {
            assert_eq!(run_label(state, false), label, "{state:?}");
            assert_eq!(
                run_summary(&line(state, Some("tests fail"), false)),
                summary
            );
        }
    }

    #[test]
    fn a_frozen_live_run_is_paused_and_a_summary_is_one_short_line() {
        for state in [RunState::Starting, RunState::Running, RunState::Checking] {
            assert_eq!(run_label(state, true), "paused");
            assert_eq!(
                run_summary(&line(state, None, true)),
                "fix the login test: frozen by a pause, continues on resume"
            );
        }
        assert_eq!(run_label(RunState::Clean, true), "ready for review");

        let long = format!("first\nsecond\u{2028}\u{7}{}", "x".repeat(300));
        let summary = run_summary(&line(RunState::NeedsYou, Some(&long), false));
        assert!(
            summary.starts_with("fix the login test: first second  x"),
            "{summary}"
        );
        assert_eq!(summary.chars().count(), SUMMARY_MAX_CHARS);
        assert!(!summary.chars().any(char::is_control));
        assert_eq!(
            run_summary(&line(RunState::NeedsYou, None, false)),
            "fix the login test: no reason given"
        );
    }
}
