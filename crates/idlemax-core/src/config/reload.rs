//! Domain: which settings that changed in `config.toml` a running agent can apply at once and
//! which it reads only when a process starts. Pure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::settings::json;
use super::{Config, ConfigError};

/// Settings the agent applies on reload, besides every `guard.*` key. Anything not listed here
/// or under `coordinator.` and `advisor.` is read at agent start, so a setting added later is
/// reported as needing a restart until someone makes it live.
const LIVE: [&str; 2] = ["agent.command_allowlist", "agent.notify"];

/// What a reload did, by setting name (`section.name`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadReport {
    /// Applied now by the running agent.
    pub applied: Vec<String>,
    /// Changed in the file; the agent reads these only when it starts.
    pub restart_agent: Vec<String>,
    /// Changed in the file; the coordinator reads these only when it starts.
    pub restart_coordinator: Vec<String>,
}

impl ReloadReport {
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
            && self.restart_agent.is_empty()
            && self.restart_coordinator.is_empty()
    }

    /// True when the agent must rebuild its guard policy.
    pub fn guard_changed(&self) -> bool {
        self.applied.iter().any(|key| key.starts_with("guard."))
    }

    pub fn applies(&self, key: &str) -> bool {
        self.applied.iter().any(|applied| applied == key)
    }
}

/// Sorts every setting that differs between `old` and `new` into applied or needs a restart.
pub fn plan_reload(old: &Config, new: &Config) -> Result<ReloadReport, ConfigError> {
    let (old, new) = (flatten(old)?, flatten(new)?);
    let mut report = ReloadReport::default();
    for key in old
        .keys()
        .chain(new.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if old.get(key) == new.get(key) {
            continue;
        }
        let list = if key.starts_with("guard.") || LIVE.contains(&key.as_str()) {
            &mut report.applied
        } else if key.starts_with("coordinator.") || key.starts_with("advisor.") {
            &mut report.restart_coordinator
        } else {
            &mut report.restart_agent
        };
        list.push(key.clone());
    }
    Ok(report)
}

/// `section.name` for every setting, with the `[guard]` overrides as the file wrote them.
fn flatten(config: &Config) -> Result<BTreeMap<String, Value>, ConfigError> {
    let mut flat = BTreeMap::new();
    if let Some(sections) = json(config)?.as_object() {
        for (section, body) in sections {
            for (name, value) in body.as_object().into_iter().flatten() {
                flat.insert(format!("{section}.{name}"), value.clone());
            }
        }
    }
    Ok(flat)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config::parse(text).unwrap()
    }

    #[test]
    fn a_reload_applies_the_guard_and_the_allowlist_and_names_what_needs_a_restart() {
        let old = config("");
        let new = config(
            "[guard]\nrequire_ac_power = false\n[agent]\ncommand_allowlist = [\"git\"]\nnotify = true\nsample_secs = 10\n[coordinator]\nlease_secs = 60\n[advisor]\nenabled = false\n",
        );
        let report = plan_reload(&old, &new).unwrap();
        assert_eq!(
            report,
            ReloadReport {
                applied: vec![
                    "agent.command_allowlist".into(),
                    "agent.notify".into(),
                    "guard.require_ac_power".into(),
                ],
                restart_agent: vec!["agent.sample_secs".into()],
                restart_coordinator: vec![
                    "advisor.enabled".into(),
                    "coordinator.lease_secs".into()
                ],
            }
        );
        assert!(report.guard_changed() && report.applies("agent.notify"));

        // A key removed from the file is a change too, and an identical file changes nothing.
        let back = plan_reload(&new, &old).unwrap();
        assert!(back.applies("guard.require_ac_power"));
        assert!(plan_reload(&new, &new).unwrap().is_empty());
    }
}
