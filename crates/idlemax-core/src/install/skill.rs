//! Domain: where the idlemax skill goes for each coding agent on this machine, and whether each
//! copy matches this binary, so every agent the user talks to knows how idlemax works and how
//! to bring its finished work back to the user.

use std::path::{Path, PathBuf};

use crate::doctor::Check;

/// The skill's folder under an agent's config directory.
pub const SKILL_FILE: &str = "skills/idlemax/SKILL.md";

/// One agent's copy of the skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub agent: &'static str,
    pub file: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyState {
    Missing,
    Stale,
    Current,
}

/// The copies to keep, one per agent whose config directory exists (`None` when it does not).
/// OpenCode also reads Claude Code's skills folder, so it gets its own copy only when there is
/// no Claude Code folder; two copies would show the skill twice.
pub fn targets(
    claude: Option<&Path>,
    codex: Option<&Path>,
    opencode: Option<&Path>,
) -> Vec<Target> {
    let at = |agent, dir: &Path| Target {
        agent,
        file: dir.join(SKILL_FILE),
    };
    let mut out = Vec::new();
    out.extend(claude.map(|d| at("claude", d)));
    out.extend(codex.map(|d| at("codex", d)));
    if claude.is_none() {
        out.extend(opencode.map(|d| at("opencode", d)));
    }
    out
}

pub fn copy_state(existing: Option<&str>, generated: &str) -> CopyState {
    match existing {
        None => CopyState::Missing,
        Some(text) if text == generated => CopyState::Current,
        Some(_) => CopyState::Stale,
    }
}

/// The doctor check: every agent found has the current skill, or which ones do not.
pub fn check(copies: &[(Target, CopyState)]) -> Check {
    const FIX: &str = "idlemax skill install";
    if copies.is_empty() {
        return Check::ok("skill", "no Claude Code, Codex or OpenCode folder found");
    }
    let off: Vec<String> = copies
        .iter()
        .filter(|(_, s)| *s != CopyState::Current)
        .map(|(t, s)| {
            let what = if *s == CopyState::Missing {
                "missing"
            } else {
                "out of date"
            };
            format!("{} ({what})", t.agent)
        })
        .collect();
    if off.is_empty() {
        let names: Vec<&str> = copies.iter().map(|(t, _)| t.agent).collect();
        return Check::ok("skill", format!("current for {}", names.join(", ")));
    }
    Check::warn(
        "skill",
        format!(
            "{}: without the current idlemax skill, the agent cannot tell you what idlemax did or review it",
            off.join(", ")
        ),
        FIX,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Level;

    #[test]
    fn each_agent_found_gets_one_copy_and_opencode_reads_claude_s() {
        let (c, x, o) = (
            Path::new("/h/.claude"),
            Path::new("/h/.codex"),
            Path::new("/h/oc"),
        );
        let names = |t: Vec<Target>| t.into_iter().map(|t| t.agent).collect::<Vec<_>>();
        assert_eq!(
            names(targets(Some(c), Some(x), Some(o))),
            ["claude", "codex"]
        );
        assert_eq!(names(targets(None, None, Some(o))), ["opencode"]);
        assert!(targets(None, None, None).is_empty());
        assert_eq!(
            targets(Some(c), None, None)[0].file,
            Path::new("/h/.claude/skills/idlemax/SKILL.md")
        );
    }

    #[test]
    fn doctor_warns_for_a_missing_or_stale_copy_and_names_the_install() {
        assert_eq!(copy_state(None, "a"), CopyState::Missing);
        assert_eq!(copy_state(Some("old"), "a"), CopyState::Stale);
        assert_eq!(copy_state(Some("a"), "a"), CopyState::Current);
        let t = |agent| Target {
            agent,
            file: PathBuf::new(),
        };
        let ok = check(&[(t("claude"), CopyState::Current)]);
        assert_eq!(ok.level, Level::Ok);
        let warn = check(&[
            (t("claude"), CopyState::Current),
            (t("codex"), CopyState::Stale),
        ]);
        assert_eq!(warn.level, Level::Warn);
        assert!(
            warn.detail.contains("codex (out of date)"),
            "{}",
            warn.detail
        );
        assert_eq!(warn.fix.as_deref(), Some("idlemax skill install"));
    }
}
