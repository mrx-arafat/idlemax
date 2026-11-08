//! Domain: wrapping Claude Code's `statusLine.command` with the capture script, and undoing it.
//! Pure text in, text out. Only the command value (or the whole `statusLine` member when there
//! was none) is touched, so every other byte of the user's `settings.json` survives.

use std::path::Path;

use serde_json::Value;

use super::json_edit::{parse_object, Member};
use super::quote::{read_shell_word, shell_quote};

/// File name of the capture script; also how an installed wrapper is recognised.
pub const WRAPPER_NAME: &str = "claude-statusline-capture.sh";
pub const WRAPPER_SCRIPT: &str = include_str!("../../../../packaging/claude-statusline-capture.sh");
const HOME_VAR: &str = "IDLEMAX_HOME";
const SHELL_PREFIX: &str = "sh -c ";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatuslineError {
    #[error("not valid JSON ({0}); fix it by hand, then run this again")]
    InvalidJson(String),
    #[error("{0}; idlemax will not guess, edit it by hand")]
    UnexpectedShape(String),
    #[error("internal check failed ({0}); settings.json was left unchanged")]
    Verify(String),
}

fn shape<T>(msg: impl Into<String>) -> Result<T, StatuslineError> {
    Err(StatuslineError::UnexpectedShape(msg.into()))
}

/// How the wrapper is invoked: its absolute path, and the state dir when it is not the default.
#[derive(Debug, Clone, Copy)]
pub struct Wrapper<'a> {
    pub script: &'a str,
    pub idlemax_home: Option<&'a str>,
}

/// A command this module wrote (or the manual form the README used to describe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    pub script: String,
    pub idlemax_home: Option<String>,
    /// The command that was there before; `None` when idlemax added the whole `statusLine`.
    pub original: Option<String>,
}

/// What `settings.json` holds now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wiring {
    NoSettings,
    NoStatusLine,
    Plain { command: String },
    Wrapped(Wrapped),
}

/// `[IDLEMAX_HOME=<dir> ]<script>[ sh -c '<original>']`. The original runs under `sh -c`
/// because the script executes its arguments directly, which would break shell syntax.
pub fn wrapped_command(w: &Wrapper, original: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(home) = w.idlemax_home {
        out.push_str(&format!("{HOME_VAR}={} ", shell_quote(home)));
    }
    out.push_str(&shell_quote(w.script));
    if let Some(original) = original {
        out.push_str(&format!(" {SHELL_PREFIX}{}", shell_quote(original)));
    }
    out
}

/// Inverse of `wrapped_command`; `None` when the command does not start with the capture script.
pub fn parse_wrapped(command: &str) -> Option<Wrapped> {
    let mut rest = command.trim();
    let mut idlemax_home = None;
    if let Some(after) = rest.strip_prefix("IDLEMAX_HOME=") {
        let (home, tail) = read_shell_word(after)?;
        idlemax_home = Some(home);
        rest = tail;
    }
    let (script, tail) = read_shell_word(rest)?;
    if Path::new(&script).file_name()? != WRAPPER_NAME {
        return None;
    }
    let tail = tail.trim();
    let original = if tail.is_empty() {
        None
    } else {
        match tail.strip_prefix(SHELL_PREFIX).and_then(read_shell_word) {
            Some((word, "")) => Some(word),
            _ => Some(tail.to_string()),
        }
    };
    Some(Wrapped {
        script,
        idlemax_home,
        original,
    })
}

struct StatusLine {
    member: usize,
    command: String,
    command_value: std::ops::Range<usize>,
    only_ours: bool,
}

struct Doc<'a> {
    text: &'a str,
    root: std::ops::Range<usize>,
    members: Vec<Member>,
    line: Option<StatusLine>,
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

fn only_one<'a>(
    members: &'a [Member],
    key: &str,
) -> Result<Option<(usize, &'a Member)>, StatuslineError> {
    let mut found = members.iter().enumerate().filter(|(_, m)| m.key == key);
    let first = found.next();
    if found.next().is_some() {
        return shape(format!("it has more than one \"{key}\" key"));
    }
    Ok(first)
}

fn read(text: &str) -> Result<Doc<'_>, StatuslineError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| StatuslineError::InvalidJson(e.to_string()))?;
    let Value::Object(root_map) = &value else {
        return shape(format!(
            "the top level is {}, expected a JSON object",
            kind(&value)
        ));
    };
    let scan = |e| StatuslineError::Verify(format!("{e}"));
    let (members, root) = parse_object(text, 0).map_err(scan)?;
    let line = match only_one(&members, "statusLine")? {
        None => None,
        Some((member, m)) => {
            let Value::Object(map) = &root_map["statusLine"] else {
                return shape(format!(
                    "\"statusLine\" is {}, expected an object like {{\"type\": \"command\", \"command\": \"...\"}}",
                    kind(&root_map["statusLine"])
                ));
            };
            if map.get("type").and_then(Value::as_str) != Some("command") {
                return shape("\"statusLine.type\" is not \"command\"");
            }
            let Some(command) = map
                .get("command")
                .and_then(Value::as_str)
                .filter(|c| !c.trim().is_empty())
            else {
                return shape("\"statusLine\" has no \"command\" string");
            };
            let (inner, _) = parse_object(text, m.value.start).map_err(scan)?;
            let Some((_, c)) = only_one(&inner, "command")? else {
                return shape("\"statusLine\" has no \"command\" string");
            };
            Some(StatusLine {
                member,
                command: command.to_string(),
                command_value: c.value.clone(),
                only_ours: inner.iter().all(|m| m.key == "type" || m.key == "command"),
            })
        }
    };
    Ok(Doc {
        text,
        root,
        members,
        line,
    })
}

/// Reports how `settings.json` is wired (`None` means the file does not exist).
pub fn inspect(text: Option<&str>) -> Result<Wiring, StatuslineError> {
    let Some(text) = text else {
        return Ok(Wiring::NoSettings);
    };
    Ok(match read(text)?.line {
        None => Wiring::NoStatusLine,
        Some(l) => match parse_wrapped(&l.command) {
            Some(w) => Wiring::Wrapped(w),
            None => Wiring::Plain { command: l.command },
        },
    })
}

fn json_string(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

fn replace(text: &str, range: std::ops::Range<usize>, with: &str) -> String {
    format!("{}{with}{}", &text[..range.start], &text[range.end..])
}

/// `"statusLine": {...}` laid out like the rest of the file: multi-line with `indent`, or inline.
fn member_text(command: &str, indent: Option<&str>) -> String {
    let command = json_string(command);
    match indent {
        Some(i) => format!(
            "\"statusLine\": {{\n{i}{i}\"type\": \"command\",\n{i}{i}\"command\": {command}\n{i}}}"
        ),
        None => format!("\"statusLine\": {{\"type\": \"command\", \"command\": {command}}}"),
    }
}

/// Whitespace before the first member when the object spans lines, else `None`.
fn indent_of(doc: &Doc) -> Option<String> {
    let first = doc.members.first()?;
    let gap = &doc.text[doc.root.start + 1..first.start];
    gap.rfind('\n').map(|i| gap[i + 1..].to_string())
}

fn add_statusline(doc: &Doc, command: &str) -> String {
    let text = doc.text;
    let Some(last) = doc.members.last() else {
        let new = format!("{{\n  {}\n}}", member_text(command, Some("  ")));
        return replace(text, doc.root.clone(), &new);
    };
    let at = last.value.end;
    let insert = match indent_of(doc) {
        Some(i) => format!(",\n{i}{}", member_text(command, Some(&i))),
        None => format!(", {}", member_text(command, None)),
    };
    replace(text, at..at, &insert)
}

fn remove_member(doc: &Doc, idx: usize) -> String {
    let (members, text) = (&doc.members, doc.text);
    let this = &members[idx];
    if members.len() == 1 {
        return replace(text, doc.root.clone(), "{}");
    }
    if idx + 1 == members.len() {
        return replace(text, members[idx - 1].value.end..this.value.end, "");
    }
    replace(text, this.start..members[idx + 1].start, "")
}

/// The new `settings.json` text with the statusline wrapped; the same text when it already is.
pub fn plan_install(text: Option<&str>, w: &Wrapper) -> Result<String, StatuslineError> {
    let out = match text {
        None => format!(
            "{{\n  {}\n}}\n",
            member_text(&wrapped_command(w, None), Some("  "))
        ),
        Some(text) => {
            let doc = read(text)?;
            match &doc.line {
                None => add_statusline(&doc, &wrapped_command(w, None)),
                Some(l) if parse_wrapped(&l.command).is_some() => return Ok(text.to_string()),
                Some(l) => {
                    let command = wrapped_command(w, Some(&l.command));
                    replace(text, l.command_value.clone(), &json_string(&command))
                }
            }
        }
    };
    verify(text, &out)?;
    Ok(out)
}

/// True when wrapping `backup` again gives exactly `text`, i.e. `text` is that backup plus our edit.
fn is_backup_of(backup: &str, text: &str, w: &Wrapped) -> bool {
    let spec = Wrapper {
        script: &w.script,
        idlemax_home: w.idlemax_home.as_deref(),
    };
    let backup_is_plain = !matches!(inspect(Some(backup)), Ok(Wiring::Wrapped(_)));
    backup_is_plain && plan_install(Some(backup), &spec).is_ok_and(|rewrapped| rewrapped == text)
}

/// The new `settings.json` text with the original command back; the same text when not wrapped.
/// `backup` is the copy made at install: when `text` is exactly that copy plus our edit, the
/// copy is restored byte for byte, otherwise only the command is edited back.
pub fn plan_uninstall(text: &str, backup: Option<&str>) -> Result<String, StatuslineError> {
    let doc = read(text)?;
    let Some(l) = &doc.line else {
        return Ok(text.to_string());
    };
    let Some(wrapped) = parse_wrapped(&l.command) else {
        return Ok(text.to_string());
    };
    if let Some(backup) = backup.filter(|b| is_backup_of(b, text, &wrapped)) {
        return Ok(backup.to_string());
    }
    let out = match wrapped.original {
        Some(original) => replace(text, l.command_value.clone(), &json_string(&original)),
        None if l.only_ours => remove_member(&doc, l.member),
        None => return shape("\"statusLine\" has extra keys next to the idlemax wrapper"),
    };
    verify(Some(text), &out)?;
    Ok(out)
}

/// Safety net: the result must be valid JSON and differ from the input only in `statusLine`.
fn verify(before: Option<&str>, after: &str) -> Result<(), StatuslineError> {
    let strip = |text: &str| -> Result<Value, StatuslineError> {
        let mut v: Value =
            serde_json::from_str(text).map_err(|e| StatuslineError::Verify(e.to_string()))?;
        if let Some(map) = v.as_object_mut() {
            map.remove("statusLine");
        }
        Ok(v)
    };
    let old = match before {
        Some(t) => strip(t)?,
        None => Value::Object(Default::default()),
    };
    if old == strip(after)? {
        Ok(())
    } else {
        Err(StatuslineError::Verify(
            "a key other than statusLine changed".into(),
        ))
    }
}

#[cfg(test)]
mod tests;
