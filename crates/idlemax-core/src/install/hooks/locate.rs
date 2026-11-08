//! Domain: finds our hook inside a Claude Code or Codex hooks file by byte span, after
//! checking that the file has the shape those tools document. Pure; never edits.

use std::ops::Range;

use serde_json::Value;

use super::{is_ours, scan, shape, HooksError, EVENT};
use crate::install::json_edit::{parse_array, parse_object, Member};

/// A JSON container located by byte span: its range with brackets, and its items' spans.
pub(super) struct Seq {
    pub(super) range: Range<usize>,
    pub(super) items: Vec<Range<usize>>,
}

/// One of our hooks: where its `command` string is, and where it sits.
pub(super) struct Spot {
    pub(super) command: Range<usize>,
    pub(super) command_text: String,
    pub(super) hook: usize,
    pub(super) hooks: Seq,
    pub(super) group: usize,
    /// The group holds nothing but this one hook (and an optional `matcher`).
    pub(super) group_is_only_ours: bool,
}

pub(super) struct Located {
    pub(super) root: Range<usize>,
    pub(super) root_members: Vec<Member>,
    /// The `hooks` member: index in the root, the span of its object and its members.
    pub(super) hooks: Option<(usize, Range<usize>, Vec<Member>)>,
    /// The `SessionStart` member of `hooks`: index and its groups.
    pub(super) event: Option<(usize, Seq)>,
    pub(super) ours: Vec<Spot>,
}

fn only_one<'a>(ms: &'a [Member], key: &str) -> Result<Option<(usize, &'a Member)>, HooksError> {
    let mut found = ms.iter().enumerate().filter(|(_, m)| m.key == key);
    let first = found.next();
    if found.next().is_some() {
        return shape(format!("it has more than one \"{key}\" key"));
    }
    Ok(first)
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The structure we edit must be what Claude Code and Codex document; anything else is left alone.
fn check_shape(root: &Value) -> Result<(), HooksError> {
    let Value::Object(map) = root else {
        return shape(format!(
            "the top level is {}, expected an object",
            kind(root)
        ));
    };
    let Some(hooks) = map.get("hooks") else {
        return Ok(());
    };
    let Value::Object(hooks) = hooks else {
        return shape(format!("\"hooks\" is {}, expected an object", kind(hooks)));
    };
    let Some(event) = hooks.get(EVENT) else {
        return Ok(());
    };
    let Value::Array(groups) = event else {
        return shape(format!(
            "\"hooks.{EVENT}\" is {}, expected an array",
            kind(event)
        ));
    };
    for group in groups {
        let Value::Object(g) = group else {
            return shape(format!(
                "an entry of \"hooks.{EVENT}\" is {}, expected an object",
                kind(group)
            ));
        };
        match g.get("hooks") {
            None => {}
            Some(Value::Array(list)) if list.iter().all(Value::is_object) => {}
            Some(other) => {
                return shape(format!(
                "a \"hooks\" entry inside \"hooks.{EVENT}\" is {}, expected an array of objects",
                kind(other)
            ))
            }
        }
    }
    Ok(())
}

pub(super) fn locate(text: &str) -> Result<Located, HooksError> {
    let value: Value = serde_json::from_str(text).map_err(|e| HooksError::Invalid {
        format: "JSON",
        why: e.to_string(),
    })?;
    check_shape(&value)?;
    let (root_members, root) = parse_object(text, 0).map_err(scan)?;
    let mut located = Located {
        root,
        hooks: None,
        event: None,
        ours: Vec::new(),
        root_members,
    };
    let Some((hooks_idx, hooks)) = only_one(&located.root_members, "hooks")? else {
        return Ok(located);
    };
    let (members, range) = parse_object(text, hooks.value.start).map_err(scan)?;
    let event = only_one(&members, EVENT)?.map(|(i, m)| (i, m.value.start));
    located.hooks = Some((hooks_idx, range, members));
    let Some((event_idx, start)) = event else {
        return Ok(located);
    };
    let (groups, range) = parse_array(text, start).map_err(scan)?;
    located.ours = find_ours(text, &groups)?;
    located.event = Some((
        event_idx,
        Seq {
            range,
            items: groups,
        },
    ));
    Ok(located)
}

fn find_ours(text: &str, groups: &[Range<usize>]) -> Result<Vec<Spot>, HooksError> {
    let mut spots = Vec::new();
    for (group, g) in groups.iter().enumerate() {
        let (members, _) = parse_object(text, g.start).map_err(scan)?;
        let plain = members
            .iter()
            .all(|m| m.key == "hooks" || m.key == "matcher");
        let Some((_, list)) = only_one(&members, "hooks")? else {
            continue;
        };
        let (items, range) = parse_array(text, list.value.start).map_err(scan)?;
        for (hook, h) in items.iter().enumerate() {
            let (fields, _) = parse_object(text, h.start).map_err(scan)?;
            let Some((_, cmd)) = only_one(&fields, "command")? else {
                continue;
            };
            let Ok(command_text) = serde_json::from_str::<String>(&text[cmd.value.clone()]) else {
                continue;
            };
            if is_ours(&command_text) {
                spots.push(Spot {
                    command: cmd.value.clone(),
                    command_text,
                    hook,
                    hooks: Seq {
                        range: range.clone(),
                        items: items.clone(),
                    },
                    group,
                    group_is_only_ours: plain && items.len() == 1,
                });
            }
        }
    }
    Ok(spots)
}
