//! Domain: every setting of `config.toml` with the value in effect, its default and whether
//! the file sets it. This is what `idlemax config` shows and what `config set` checks keys
//! against. Pure.

use serde::Serialize;
use serde_json::Value;

use super::{Config, ConfigError, GuardPolicy};
use crate::types::{DeviceProfile, Os};

/// The sections that hold settings, in the order they are listed.
const SECTIONS: [&str; 5] = ["coordinator", "agent", "guard", "quota", "advisor"];

/// Laptop capacity is sized to the machine when the agent starts, so an unset value has no
/// fixed number to show.
const SIZED_AT_START: [&str; 2] = ["guard.max_jobs", "guard.ram_budget_mib"];

/// One setting. `value` null means the option or check is off.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Setting {
    pub key: String,
    pub value: Value,
    pub default: Value,
    /// True when `config.toml` sets this key.
    pub from_file: bool,
}

/// Every setting in effect for the `config.toml` text, with the `[guard]` section shown as the
/// device preset after the file's overrides. `os` picks the default device profile.
pub fn list_settings(text: &str, os: Os) -> Result<Vec<Setting>, ConfigError> {
    let cfg = Config::parse(text)?;
    let file: toml::Table = toml::from_str(text)?;
    let profile = cfg.agent.profile_or_default(os);
    let mut now = json(&cfg)?;
    let mut default = json(&Config::default())?;
    replace_section(&mut now, "guard", json(&cfg.guard_policy(os)?)?);
    replace_section(
        &mut default,
        "guard",
        json(&GuardPolicy::for_profile(profile))?,
    );

    let mut settings = Vec::new();
    for section in SECTIONS {
        let (Some(now), Some(default)) = (
            now.get(section).and_then(Value::as_object),
            default.get(section).and_then(Value::as_object),
        ) else {
            continue;
        };
        let in_file = file.get(section).and_then(toml::Value::as_table);
        for (name, value) in now {
            let key = format!("{section}.{name}");
            let from_file = in_file.is_some_and(|t| t.contains_key(name));
            let auto = !from_file
                && profile == DeviceProfile::MacosLaptop
                && SIZED_AT_START.contains(&key.as_str());
            settings.push(Setting {
                key,
                value: if auto { auto_value() } else { value.clone() },
                default: default.get(name).cloned().unwrap_or(Value::Null),
                from_file,
            });
        }
    }
    Ok(settings)
}

/// The value shown for a laptop's unset capacity.
fn auto_value() -> Value {
    Value::String("auto".into())
}

/// A value as people read it: `false`, `40`, `macos_laptop`, `["git", "make"]`; `off` for a
/// check that is disabled. Text is shown bare, except inside a list.
pub fn show_value(value: &Value) -> String {
    match value {
        Value::Null => "off".into(),
        Value::String(text) => text.clone(),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(show_item).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.to_string(),
    }
}

fn show_item(item: &Value) -> String {
    match item {
        Value::String(text) => format!("{text:?}"),
        other => show_value(other),
    }
}

pub(super) fn json<T: Serialize>(value: &T) -> Result<Value, ConfigError> {
    serde_json::to_value(value).map_err(|e| ConfigError::Guard(format!("settings: {e}")))
}

fn replace_section(all: &mut Value, name: &str, section: Value) {
    if let Some(object) = all.as_object_mut() {
        object.insert(name.into(), section);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn find<'a>(settings: &'a [Setting], key: &str) -> &'a Setting {
        settings.iter().find(|s| s.key == key).expect(key)
    }

    #[test]
    fn the_list_shows_the_value_in_effect_the_default_and_where_it_comes_from() {
        let text = "[agent]\nnotify = true\n\n[guard]\nrequire_ac_power = false\nmax_jobs = 3\n";
        let settings = list_settings(text, Os::Macos).unwrap();

        let ac = find(&settings, "guard.require_ac_power");
        assert_eq!(
            (ac.value.clone(), ac.default.clone()),
            (json!(false), json!(true))
        );
        assert!(ac.from_file);

        // A check the preset leaves off is listed too, as null.
        let swap = find(&settings, "guard.swap_used_mib_pause");
        assert_eq!(swap.value, Value::Null);
        assert_eq!(show_value(&swap.value), "off");
        assert!(!swap.from_file);

        // Laptop capacity is sized at start unless the file pins it.
        assert_eq!(find(&settings, "guard.max_jobs").value, json!(3));
        let ram = find(&settings, "guard.ram_budget_mib");
        assert_eq!(
            (ram.value.clone(), ram.default.clone()),
            (json!("auto"), json!(1024))
        );
        let server = list_settings("[agent]\nprofile = \"linux_server\"\n", Os::Macos).unwrap();
        assert_eq!(find(&server, "guard.max_jobs").value, json!(1));

        assert_eq!(
            show_value(&find(&settings, "agent.command_allowlist").value),
            "[]"
        );
        assert!(find(&settings, "agent.notify").from_file);
    }
}
