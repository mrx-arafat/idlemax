//! Domain: the SessionStart hook entry of `idlemax hooks`, as an edit of Claude Code's
//! `settings.json` or Codex's `hooks.json` (both use `hooks.SessionStart[].hooks[]`).
//! Pure text in, text out. Only our own entry is added, repaired or removed, so every other
//! byte of the user's file survives. Sibling modules cover Codex's feature flag and OpenCode.

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::json_edit::ScanError;
use super::quote::{read_shell_word, shell_quote};
use locate::{locate, Located, Spot};

mod locate;

/// What the hook runs after the program: `idlemax hooks summary`.
pub const SUBCOMMAND: &str = "hooks summary";
pub const BINARY: &str = "idlemax";
/// A session start must never wait long on this hook.
pub const TIMEOUT_SECS: u32 = 10;
const EVENT: &str = "SessionStart";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HooksError {
    #[error("not valid {format} ({why}); fix it by hand, then run this again")]
    Invalid { format: &'static str, why: String },
    #[error("{0}; idlemax will not guess, edit it by hand")]
    UnexpectedShape(String),
    #[error("internal check failed ({0}); the file was left unchanged")]
    Verify(String),
}

fn shape<T>(msg: impl Into<String>) -> Result<T, HooksError> {
    Err(HooksError::UnexpectedShape(msg.into()))
}

fn scan(e: ScanError) -> HooksError {
    HooksError::Verify(e.to_string())
}

/// What a hooks file holds for us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookState {
    NoFile,
    NotInstalled,
    Installed { command: String },
}

/// `<program> hooks summary`, with the program quoted for a shell.
pub fn hook_command(program: &str) -> String {
    format!("{} {SUBCOMMAND}", shell_quote(program))
}

/// The program to put in the hook: the bare name when PATH resolves it to this very executable
/// (portable across reinstalls), else the absolute path so the hook never runs another binary.
pub fn program_for(exe: &Path, on_path: Option<&Path>) -> String {
    match on_path {
        Some(found) if found == exe => BINARY.to_string(),
        _ => exe.display().to_string(),
    }
}

/// Where a bare program name resolves, from a `PATH` value; the adapter canonicalises it.
pub fn find_on_path(path_var: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// True for a command this module wrote: any path to `idlemax`, then `hooks summary`.
pub fn is_ours(command: &str) -> bool {
    let Some((program, rest)) = read_shell_word(command) else {
        return false;
    };
    Path::new(&program).file_name().is_some_and(|n| n == BINARY) && rest.trim() == SUBCOMMAND
}

/// Reports what the file holds (`None` means the file does not exist).
pub fn inspect(text: Option<&str>) -> Result<HookState, HooksError> {
    let Some(text) = text else {
        return Ok(HookState::NoFile);
    };
    Ok(match locate(text)?.ours.first() {
        Some(spot) => HookState::Installed {
            command: spot.command_text.clone(),
        },
        None => HookState::NotInstalled,
    })
}

// ---- installing ----------------------------------------------------------------------------

fn json_string(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

fn replace(text: &str, range: Range<usize>, with: &str) -> String {
    format!("{}{with}{}", &text[..range.start], &text[range.end..])
}

/// One level of indentation: what the file's first root member uses, else two spaces.
fn unit_of(text: &str, l: &Located) -> String {
    let gap = l
        .root_members
        .first()
        .map(|m| &text[l.root.start + 1..m.start])
        .and_then(|g| g.rfind('\n').map(|i| &g[i + 1..]));
    gap.filter(|g| !g.is_empty()).unwrap_or("  ").to_string()
}

fn group_json(command: &str, indent: Option<(&str, &str)>) -> String {
    let (hook, entry) = (json_string(command), TIMEOUT_SECS);
    let Some((i, u)) = indent else {
        return format!(
            "{{\"matcher\": \"\", \"hooks\": [{{\"type\": \"command\", \"command\": {hook}, \"timeout\": {entry}}}]}}"
        );
    };
    format!(
        "{{\n{i}{u}\"matcher\": \"\",\n{i}{u}\"hooks\": [\n{i}{u}{u}{{\n{i}{u}{u}{u}\"type\": \"command\",\n{i}{u}{u}{u}\"command\": {hook},\n{i}{u}{u}{u}\"timeout\": {entry}\n{i}{u}{u}}}\n{i}{u}]\n{i}}}"
    )
}

fn event_member(command: &str, indent: Option<(&str, &str)>) -> String {
    match indent {
        None => format!("\"{EVENT}\": [{}]", group_json(command, None)),
        Some((i, u)) => {
            let inner = format!("{i}{u}");
            format!(
                "\"{EVENT}\": [\n{inner}{}\n{i}]",
                group_json(command, Some((&inner, u)))
            )
        }
    }
}

fn hooks_member(command: &str, indent: Option<(&str, &str)>) -> String {
    match indent {
        None => format!("\"hooks\": {{{}}}", event_member(command, None)),
        Some((i, u)) => {
            let inner = format!("{i}{u}");
            format!(
                "\"hooks\": {{\n{inner}{}\n{i}}}",
                event_member(command, Some((&inner, u)))
            )
        }
    }
}

/// Appends `inline` (or `multi(indent)` when the container is laid out one item per line)
/// after the container's last item; an empty container gets the inline form.
fn append(
    text: &str,
    range: &Range<usize>,
    first_last: Option<(usize, usize)>,
    inline: &str,
    multi: impl Fn(&str) -> String,
) -> String {
    let Some((first, last)) = first_last else {
        let (open, close) = (
            &text[range.start..=range.start],
            &text[range.end - 1..range.end],
        );
        return replace(text, range.clone(), &format!("{open}{inline}{close}"));
    };
    let gap = &text[range.start + 1..first];
    match gap.rfind('\n') {
        Some(i) => {
            let indent = &gap[i + 1..];
            replace(text, last..last, &format!(",\n{indent}{}", multi(indent)))
        }
        None => replace(text, last..last, &format!(", {inline}")),
    }
}

/// The new file text with our hook added or its command repaired; the same text when current.
pub fn plan_install(text: Option<&str>, command: &str) -> Result<String, HooksError> {
    let Some(text) = text else {
        let unit = "  ";
        return Ok(format!(
            "{{\n{unit}{}\n}}\n",
            hooks_member(command, Some((unit, unit)))
        ));
    };
    let l = locate(text)?;
    let u = unit_of(text, &l);
    let out = if !l.ours.is_empty() {
        if l.ours.iter().all(|spot| spot.command_text == command) {
            return Ok(text.to_string());
        }
        // Path repair: only the command strings change. Last first, so earlier spans stay valid.
        l.ours.iter().rev().fold(text.to_string(), |acc, spot| {
            replace(&acc, spot.command.clone(), &json_string(command))
        })
    } else if let Some((_, seq)) = &l.event {
        let first_last = seq
            .items
            .first()
            .zip(seq.items.last())
            .map(|(f, e)| (f.start, e.end));
        append(
            text,
            &seq.range,
            first_last,
            &group_json(command, None),
            |i| group_json(command, Some((i, &u))),
        )
    } else if let Some((_, range, members)) = &l.hooks {
        let first_last = members
            .first()
            .zip(members.last())
            .map(|(f, e)| (f.start, e.value.end));
        append(text, range, first_last, &event_member(command, None), |i| {
            event_member(command, Some((i, &u)))
        })
    } else {
        let first_last = l
            .root_members
            .first()
            .zip(l.root_members.last())
            .map(|(f, e)| (f.start, e.value.end));
        append(
            text,
            &l.root,
            first_last,
            &hooks_member(command, None),
            |i| hooks_member(command, Some((i, &u))),
        )
    };
    verify(Some(text), &out)?;
    Ok(out)
}

// ---- uninstalling --------------------------------------------------------------------------

/// Byte spans of a container's items, for removing one together with its comma.
type Spans = Vec<(usize, usize)>;

/// `text` without item `idx` of the container; a lone item leaves an empty container.
fn remove_item(text: &str, container: &Range<usize>, spans: &Spans, idx: usize) -> String {
    if spans.len() == 1 {
        let (open, close) = (
            &text[container.start..=container.start],
            &text[container.end - 1..container.end],
        );
        return replace(text, container.clone(), &format!("{open}{close}"));
    }
    if idx + 1 == spans.len() {
        return replace(text, spans[idx - 1].1..spans[idx].1, "");
    }
    replace(text, spans[idx].0..spans[idx + 1].0, "")
}

fn array_spans(items: &[Range<usize>]) -> Spans {
    items.iter().map(|r| (r.start, r.end)).collect()
}

fn member_spans(members: &[super::json_edit::Member]) -> Spans {
    members.iter().map(|m| (m.start, m.value.end)).collect()
}

/// Removes one of our hooks, and each container that held nothing else.
fn remove_spot(text: &str, l: &Located, spot: &Spot) -> String {
    let (Some((hooks_idx, hooks_range, hooks_members)), Some((event_idx, groups))) =
        (&l.hooks, &l.event)
    else {
        return text.to_string();
    };
    if !spot.group_is_only_ours {
        return remove_item(
            text,
            &spot.hooks.range,
            &array_spans(&spot.hooks.items),
            spot.hook,
        );
    }
    if groups.items.len() > 1 {
        return remove_item(text, &groups.range, &array_spans(&groups.items), spot.group);
    }
    if hooks_members.len() > 1 {
        return remove_item(text, hooks_range, &member_spans(hooks_members), *event_idx);
    }
    remove_item(text, &l.root, &member_spans(&l.root_members), *hooks_idx)
}

/// True when installing into `backup` gives exactly `text`, i.e. `text` is that backup plus our edit.
fn is_backup_of(backup: &str, text: &str, command: &str) -> bool {
    matches!(inspect(Some(backup)), Ok(HookState::NotInstalled))
        && plan_install(Some(backup), command).is_ok_and(|again| again == text)
}

/// The new file text without our hook; the same text when it is not there. `backup` is the copy
/// made at install: when `text` is exactly that copy plus our edit, the copy comes back byte for
/// byte, otherwise only our entry (and containers left empty by it) is removed.
pub fn plan_uninstall(text: &str, backup: Option<&str>) -> Result<String, HooksError> {
    let l = locate(text)?;
    let Some(first) = l.ours.first() else {
        return Ok(text.to_string());
    };
    if let Some(backup) = backup.filter(|b| is_backup_of(b, text, &first.command_text)) {
        return Ok(backup.to_string());
    }
    let mut out = text.to_string();
    loop {
        let l = locate(&out)?;
        let Some(spot) = l.ours.last() else {
            break;
        };
        out = remove_spot(&out, &l, spot);
    }
    verify(Some(text), &out)?;
    Ok(out)
}

// ---- verifying -----------------------------------------------------------------------------

/// `v` without our hooks, and without the containers that only held them.
fn without_ours(mut v: Value) -> Value {
    if let Some(Value::Object(hooks)) = v.get_mut("hooks") {
        if let Some(Value::Array(groups)) = hooks.get_mut(EVENT) {
            for g in groups.iter_mut() {
                if let Some(Value::Array(list)) = g.get_mut("hooks") {
                    list.retain(|h| !h["command"].as_str().is_some_and(is_ours));
                }
            }
            groups.retain(|g| g["hooks"].as_array().is_none_or(|l| !l.is_empty()));
            if groups.is_empty() {
                hooks.remove(EVENT);
            }
        }
    }
    if v["hooks"].as_object().is_some_and(|h| h.is_empty()) {
        if let Some(map) = v.as_object_mut() {
            map.remove("hooks");
        }
    }
    v
}

/// Safety net: the result must be valid JSON and differ from the input only by our hook.
fn verify(before: Option<&str>, after: &str) -> Result<(), HooksError> {
    let parse = |t: &str| {
        serde_json::from_str::<Value>(t)
            .map(without_ours)
            .map_err(|e| HooksError::Verify(e.to_string()))
    };
    let old = match before {
        Some(t) => parse(t)?,
        None => Value::Object(Default::default()),
    };
    if old == parse(after)? {
        Ok(())
    } else {
        Err(HooksError::Verify(
            "a key other than our hook changed".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
