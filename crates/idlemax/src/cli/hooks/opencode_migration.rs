//! Adapter: discover, migrate and remove owned OpenCode plugins across config locations.

use super::agents::{new_report, plugin_install, plugin_remove};
use super::places::{Agent, Places};
use super::{AgentReport, Outcome};
use crate::cli::files::{read_optional, write_atomic};
use anyhow::{Context, Result};
use idlemax_core::install::opencode;
use std::path::PathBuf;

fn managed_legacy(places: &Places) -> Result<Vec<PathBuf>> {
    let target = places.file(Agent::OpenCode);
    let parent = target.parent().and_then(|p| p.canonicalize().ok());
    let mut legacy = Vec::new();
    for path in &places.opencode_legacy {
        // Directory aliases name the same file; unlinking one would delete the target.
        if path == &target
            || parent.as_ref().is_some_and(|p| {
                path.parent().and_then(|p| p.canonicalize().ok()).as_ref() == Some(p)
            })
        {
            continue;
        }
        if read_optional(path)?
            .as_deref()
            .is_some_and(opencode::is_managed)
        {
            legacy.push(path.clone());
        }
    }
    Ok(legacy)
}

pub(super) fn install(places: &Places, program: &str, dry_run: bool) -> Result<AgentReport> {
    let legacy = managed_legacy(places)?;
    let target = places.file(Agent::OpenCode);
    let before = read_optional(&target)?;
    let mut report = plugin_install(&target, program, dry_run)?;
    if !legacy.is_empty() && report.outcome == Outcome::Unchanged {
        report.outcome = Outcome::Repaired;
        // Break a possible target-file symlink before removing its old backing file.
        if !dry_run {
            write_atomic(&target, opencode::plugin_source(program).as_bytes(), None)?;
        }
    }
    for path in legacy {
        if !dry_run {
            if let Err(error) = std::fs::remove_file(&path) {
                match &before {
                    Some(text) => write_atomic(&target, text.as_bytes(), None)?,
                    None => std::fs::remove_file(&target)?,
                }
                return Err(error).with_context(|| {
                    format!("removing legacy plugin {}; target restored", path.display())
                });
            }
        }
        report.notes.push(format!(
            "{} legacy plugin {} after writing {}",
            if dry_run { "would remove" } else { "removed" },
            path.display(),
            target.display()
        ));
    }
    Ok(report)
}

pub(super) fn uninstall(places: &Places, dry_run: bool) -> Result<AgentReport> {
    let mut report = new_report(
        Agent::OpenCode,
        places.file(Agent::OpenCode),
        Outcome::NotInstalled,
    );
    for path in places.opencode_files() {
        let removal = plugin_remove(&path, dry_run)?;
        if removal.outcome == Outcome::Removed {
            if report.outcome != Outcome::Removed {
                report.path = path.clone();
            }
            report.outcome = Outcome::Removed;
            report.notes.push(format!(
                "{} managed plugin {}",
                if dry_run { "would remove" } else { "removed" },
                path.display()
            ));
        }
        report.notes.extend(
            removal
                .notes
                .into_iter()
                .map(|note| format!("{}: {note}", path.display())),
        );
    }
    Ok(report)
}

pub(super) fn status(places: &Places) -> Result<AgentReport> {
    let legacy = managed_legacy(places)?;
    let target = places.file(Agent::OpenCode);
    let mut report = super::status::status_of(Agent::OpenCode, places, &target)?;
    if let Some(path) = legacy.first() {
        if report.outcome == Outcome::NotInstalled {
            report = super::status::status_of(Agent::OpenCode, places, path)?;
        }
        report.outcome = Outcome::Stale;
        for path in legacy {
            report.notes.push(format!(
                "legacy managed plugin {}; run idlemax hooks install --opencode to migrate to {}",
                path.display(),
                target.display()
            ));
        }
    } else if !places.found(Agent::OpenCode) {
        report.outcome = Outcome::NotFound;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_and_uninstall_preserve_unmanaged_files_and_refused_targets() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old/plugins/idlemax.js");
        let user = temp.path().join("other/plugins/idlemax.js");
        for file in [&old, &user] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        }
        let source = opencode::plugin_source("/bin/echo");
        std::fs::write(&old, &source).unwrap();
        std::fs::write(&user, "user plugin").unwrap();
        let places = Places {
            claude: temp.path().join("claude"),
            codex: temp.path().join("codex"),
            opencode: temp.path().join("new"),
            opencode_legacy: vec![old.clone(), user.clone()],
        };
        assert!(places.found(Agent::OpenCode));
        let target = places.file(Agent::OpenCode);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "user target").unwrap();
        assert!(install(&places, "/bin/echo", false).is_err());
        assert!(old.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "user target");
        std::fs::remove_file(&target).unwrap();
        install(&places, "/bin/echo", true).unwrap();
        assert!(old.exists() && !target.exists());
        install(&places, "/bin/echo", false).unwrap();
        assert!(target.exists() && !old.exists());
        std::fs::write(&old, &source).unwrap();
        uninstall(&places, true).unwrap();
        assert!(target.exists() && old.exists());
        uninstall(&places, false).unwrap();
        assert!(!target.exists() && !old.exists());
        assert_eq!(std::fs::read_to_string(&user).unwrap(), "user plugin");
        #[cfg(unix)]
        {
            std::fs::write(&old, &source).unwrap();
            std::os::unix::fs::symlink(&old, &target).unwrap();
            install(&places, "/bin/echo", false).unwrap();
            assert!(target.is_file() && !target.is_symlink() && !old.exists());
            uninstall(&places, false).unwrap();
            std::fs::write(&old, &source).unwrap();
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(temp.path().join("old"), &alias).unwrap();
            let alias_places = Places {
                opencode: alias,
                ..places
            };
            install(&alias_places, "/bin/echo", false).unwrap();
            assert!(
                old.exists(),
                "directory alias cleanup deleted the selected plugin"
            );
        }
    }
}
