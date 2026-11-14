//! Adapter: writes the generated skill into each coding agent's skills folder, and reads the
//! copies back for doctor. `idlemax_core::install::skill` decides where and whether.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use idlemax_core::install::skill::{copy_state, targets, CopyState, Target};
use serde::Serialize;

use crate::cli::files::read_optional;
use crate::cli::hooks::places::Places;

/// What happened to one agent's copy.
#[derive(Debug, Serialize)]
pub struct Installed {
    pub agent: &'static str,
    pub file: PathBuf,
    /// written, current, or would_write on a dry run.
    pub status: &'static str,
}

/// The copies for the agents present, each with how it compares to `generated`.
fn found(dir: &Path) -> Option<&Path> {
    dir.is_dir().then_some(dir)
}

pub fn copies(places: &Places, generated: &str) -> Result<Vec<(Target, CopyState)>> {
    let list = targets(
        found(&places.claude),
        found(&places.codex),
        found(&places.opencode),
    );
    list.into_iter()
        .map(|t| {
            let state = copy_state(read_optional(&t.file)?.as_deref(), generated);
            Ok((t, state))
        })
        .collect()
}

/// Brings every copy up to date; with `dry_run` only says what it would write.
pub fn install(places: &Places, generated: &str, dry_run: bool) -> Result<Vec<Installed>> {
    let mut out = Vec::new();
    for (target, state) in copies(places, generated)? {
        let status = match (state, dry_run) {
            (CopyState::Current, _) => "current",
            (_, true) => "would_write",
            (_, false) => {
                write(&target.file, generated)?;
                "written"
            }
        };
        out.push(Installed {
            agent: target.agent,
            file: target.file,
            status,
        });
    }
    Ok(out)
}

fn write(file: &Path, text: &str) -> Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    std::fs::write(file, text).with_context(|| format!("write {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(root: &Path) -> Places {
        Places {
            claude: root.join(".claude"),
            codex: root.join(".codex"),
            opencode: root.join("opencode"),
            opencode_legacy: Vec::new(),
        }
    }

    #[test]
    fn install_writes_each_agent_found_then_finds_them_current() {
        let dir = tempfile::tempdir().unwrap();
        let p = places(dir.path());
        std::fs::create_dir_all(&p.claude).unwrap();
        std::fs::create_dir_all(&p.codex).unwrap();
        let dry = install(&p, "skill v2", true).unwrap();
        assert!(dry.iter().all(|i| i.status == "would_write"));
        assert!(
            !p.claude.join("skills").exists(),
            "a dry run writes nothing"
        );

        let done = install(&p, "skill v2", false).unwrap();
        let got: Vec<_> = done.iter().map(|i| (i.agent, i.status)).collect();
        assert_eq!(got, [("claude", "written"), ("codex", "written")]);
        let file = p.codex.join("skills/idlemax/SKILL.md");
        assert_eq!(std::fs::read_to_string(file).unwrap(), "skill v2");
        assert!(install(&p, "skill v2", false)
            .unwrap()
            .iter()
            .all(|i| i.status == "current"));
        // An upgrade makes the copies stale until the next install.
        let after = copies(&p, "skill v3").unwrap();
        assert!(after.iter().all(|(_, s)| *s == CopyState::Stale));
    }
}
