//! Domain: change one setting in `config.toml` text and keep everything else as the user wrote
//! it (comments, order, blank lines). The result is checked the way startup checks it, so a
//! value that would stop the agent from starting is refused here. Pure.

use serde_json::Value;
use toml_edit::{Array, DocumentMut, Item, Table};

use super::settings::{list_settings, Setting};
use super::ConfigError;
use crate::types::Os;

/// Keys the command line cannot set: `[[quota.accounts]]` is a list of tables.
const NOT_EDITABLE: [&str; 1] = ["quota.accounts"];

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("config.toml is not valid yet: {0}; fix it by hand or run `idlemax doctor`")]
    Current(#[source] ConfigError),
    #[error("config.toml is not valid TOML: {0}")]
    Syntax(String),
    #[error("unknown setting `{key}`; {help}")]
    Unknown { key: String, help: String },
    #[error("`{0}` is a list of tables: edit it in config.toml by hand")]
    NotEditable(String),
    #[error("cannot set `{key}` to `{value}`: {reason}")]
    Invalid {
        key: String,
        value: String,
        reason: String,
    },
}

/// The outcome of one change. `text` is the new file; it is the old text when nothing changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub text: String,
    /// The value in effect before and after (null means the check is off).
    pub before: Value,
    pub after: Value,
    pub changed: bool,
}

/// Sets `key` (`section.name`) to `raw`. A list setting takes `git,cargo` or `["git", "cargo"]`;
/// a text setting takes the bare word; anything else is read as a TOML value.
pub fn set_setting(text: &str, key: &str, raw: &str, os: Os) -> Result<Edit, EditError> {
    let current = find_setting(text, key, os)?;
    let mut doc = parse_doc(text)?;
    let (section, name) = split(key)?;
    let table = table_of(&mut doc, section)?;
    let value = parse_value(raw, &current.default);
    match table.get_mut(name) {
        Some(old) => {
            let mut value = value;
            if let Some(old) = old.as_value() {
                *value.decor_mut() = old.decor().clone();
            }
            *old = Item::Value(value);
        }
        None => {
            table.insert(name, Item::Value(value));
        }
    }
    finish(text, doc.to_string(), key, raw, os, &current, false)
}

/// Removes `key` from the file so the preset default applies again, even when the value it
/// pinned is the default. A key the file does not set is already at its default, and nothing
/// changes.
pub fn unset_setting(text: &str, key: &str, os: Os) -> Result<Edit, EditError> {
    let current = find_setting(text, key, os)?;
    if !current.from_file {
        return Ok(Edit {
            text: text.to_string(),
            before: current.value.clone(),
            after: current.value,
            changed: false,
        });
    }
    let mut doc = parse_doc(text)?;
    let (section, name) = split(key)?;
    table_of(&mut doc, section)?.remove(name);
    finish(text, doc.to_string(), key, "unset", os, &current, true)
}

/// The setting named `key`, or the error `set` would give: unknown (with the nearest real
/// name) or not editable.
pub fn find_setting(text: &str, key: &str, os: Os) -> Result<Setting, EditError> {
    let all = list_settings(text, os).map_err(EditError::Current)?;
    let found = all.iter().find(|s| s.key == key).cloned();
    if NOT_EDITABLE.contains(&key) {
        return Err(EditError::NotEditable(key.to_string()));
    }
    found.ok_or_else(|| unknown(key, &all))
}

/// Checks `new` the way startup does. `changed` is about the value in effect; the file is
/// rewritten when the value changed, or when `rewrite` asks for it (removing a pinned key).
fn finish(
    old: &str,
    new: String,
    key: &str,
    shown: &str,
    os: Os,
    current: &Setting,
    rewrite: bool,
) -> Result<Edit, EditError> {
    let after = list_settings(&new, os).map_err(|e| invalid(key, shown, &e))?;
    let value = after
        .iter()
        .find(|s| s.key == key)
        .map_or(Value::Null, |s| s.value.clone());
    let changed = value != current.value;
    Ok(Edit {
        text: if changed || rewrite {
            new
        } else {
            old.to_string()
        },
        before: current.value.clone(),
        after: value,
        changed,
    })
}

fn parse_doc(text: &str) -> Result<DocumentMut, EditError> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| EditError::Syntax(e.message().to_string()))
}

fn split(key: &str) -> Result<(&str, &str), EditError> {
    key.split_once('.').ok_or_else(|| EditError::Unknown {
        key: key.to_string(),
        help: "a setting is written section.name, such as guard.require_ac_power".into(),
    })
}

fn table_of<'a>(doc: &'a mut DocumentMut, section: &str) -> Result<&'a mut Table, EditError> {
    doc.as_table_mut()
        .entry(section)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| EditError::Syntax(format!("[{section}] in config.toml is not a table")))
}

/// `raw` as the TOML value the setting takes, judged by the setting's default.
fn parse_value(raw: &str, default: &Value) -> toml_edit::Value {
    let raw = raw.trim();
    match default {
        Value::Array(_) if !raw.starts_with('[') => {
            let mut list = Array::new();
            for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                list.push(item);
            }
            list.fmt();
            toml_edit::Value::Array(list)
        }
        Value::String(_) if !raw.starts_with('"') => toml_edit::Value::from(raw),
        _ => raw.parse().unwrap_or_else(|_| toml_edit::Value::from(raw)),
    }
}

fn invalid(key: &str, value: &str, error: &ConfigError) -> EditError {
    let reason = match error {
        ConfigError::Parse(parse) => parse.message().to_string(),
        other => other.to_string(),
    };
    let reason = reason
        .strip_prefix(&format!("{key}: "))
        .map_or_else(|| reason.clone(), str::to_string);
    EditError::Invalid {
        key: key.to_string(),
        value: value.to_string(),
        reason,
    }
}

fn unknown(key: &str, all: &[Setting]) -> EditError {
    let nearest = all.iter().map(|s| (distance(key, &s.key), &s.key)).min();
    let help = match nearest {
        _ if !key.contains('.') => {
            "a setting is written section.name, such as guard.require_ac_power".to_string()
        }
        Some((d, near)) if d <= 3 => format!("did you mean `{near}`?"),
        _ => "run `idlemax config list --all` to see them".to_string(),
    };
    EditError::Unknown {
        key: key.to_string(),
        help,
    }
}

/// Edit distance, enough to suggest the setting a typo was meant to be.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut row = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != *cb);
            row.push(substitute.min(previous[j + 1] + 1).min(row[j] + 1));
        }
        previous = row;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FILE: &str = "# idlemax config\n[agent]\nprofile = \"macos_laptop\" # keep\ncommand_allowlist = []\n\n[guard]\n# Run on battery too.\nrequire_ac_power = true # was the default\n\n[quota]\naccounts = []\n";

    fn set(text: &str, key: &str, raw: &str) -> Result<Edit, EditError> {
        set_setting(text, key, raw, Os::Macos)
    }

    #[test]
    fn a_change_touches_one_value_and_keeps_every_comment_and_other_key() {
        let edit = set(FILE, "guard.require_ac_power", "false").unwrap();
        assert!(edit.changed);
        assert_eq!(
            (edit.before.clone(), edit.after.clone()),
            (json!(true), json!(false))
        );
        assert_eq!(
            edit.text,
            FILE.replace("require_ac_power = true", "require_ac_power = false")
        );

        // A key the file lacks is added to its section, and the file still passes startup.
        let added = set(FILE, "guard.min_battery_pct", "50").unwrap();
        assert!(added.text.contains("[guard]"), "{}", added.text);
        assert!(
            added.text.contains("min_battery_pct = 50"),
            "{}",
            added.text
        );
        assert!(added
            .text
            .starts_with("# idlemax config\n[agent]\nprofile = \"macos_laptop\" # keep\n"));
        assert_eq!(added.after, json!(50.0));
    }

    #[test]
    fn a_list_takes_commas_and_setting_the_same_value_again_changes_nothing() {
        let first = set("", "agent.command_allowlist", "git, /usr/bin/make").unwrap();
        assert_eq!(first.after, json!(["git", "/usr/bin/make"]));
        assert_eq!(
            first.text,
            "[agent]\ncommand_allowlist = [\"git\", \"/usr/bin/make\"]\n"
        );
        let again = set(
            &first.text,
            "agent.command_allowlist",
            "[\"git\", \"/usr/bin/make\"]",
        )
        .unwrap();
        assert!(!again.changed);
        assert_eq!(again.text, first.text);
        // A key at its default is not written into the file.
        let same = set("", "guard.require_ac_power", "true").unwrap();
        assert!(!same.changed);
        assert_eq!(same.text, "");
        // Text settings take the bare word.
        assert_eq!(
            set("", "agent.profile", "linux_server").unwrap().after,
            json!("linux_server")
        );
    }

    #[test]
    fn a_wrong_key_or_value_is_refused_with_what_to_do_and_the_file_is_untouched() {
        let refused = |key: &str, raw: &str| set(FILE, key, raw).unwrap_err().to_string();
        assert_eq!(
            refused("agent.sample_sec", "10"),
            "unknown setting `agent.sample_sec`; did you mean `agent.sample_secs`?"
        );
        assert_eq!(
            refused("nonsense", "1"),
            "unknown setting `nonsense`; a setting is written section.name, such as guard.require_ac_power"
        );
        assert!(refused("agent.notify", "banana")
            .starts_with("cannot set `agent.notify` to `banana`: invalid type"));
        assert!(refused("guard.min_battery_pct", "high")
            .starts_with("cannot set `guard.min_battery_pct` to `high`: invalid type"));
        assert!(refused("quota.accounts", "x").contains("list of tables"));
        assert!(set("[agent\nbroken", "agent.notify", "true")
            .unwrap_err()
            .to_string()
            .starts_with("config.toml is not valid yet"));
    }

    #[test]
    fn unset_goes_back_to_the_default_and_is_a_no_op_for_a_key_the_file_lacks() {
        let pinned = FILE.replace("require_ac_power = true", "require_ac_power = false");
        let edit = unset_setting(&pinned, "guard.require_ac_power", Os::Macos).unwrap();
        assert!(edit.changed);
        assert_eq!(
            (edit.before.clone(), edit.after.clone()),
            (json!(false), json!(true))
        );
        assert!(!edit.text.contains("require_ac_power"), "{}", edit.text);
        assert!(edit.text.contains("profile = \"macos_laptop\" # keep"));
        // A key pinned to the default is removed too, though the value stays the same.
        let tidy = unset_setting(FILE, "guard.require_ac_power", Os::Macos).unwrap();
        assert!(!tidy.changed);
        assert!(!tidy.text.contains("require_ac_power"), "{}", tidy.text);
        // A key the file lacks changes nothing at all.
        let again = unset_setting(&edit.text, "guard.require_ac_power", Os::Macos).unwrap();
        assert!(!again.changed);
        assert_eq!(again.text, edit.text);
    }
}
