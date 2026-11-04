//! Adapter: indexes recent Claude Code and Codex sessions as metadata only (title,
//! project, last activity, unfinished signals). Transcript text is scanned for signals in
//! memory and never kept. Blocking file I/O: call through `spawn_blocking`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use idlemax_core::advice::{SessionMeta, SessionSignal};
use idlemax_core::types::UnixMs;
use serde_json::Value;

const MAX_AGE_MS: i64 = 3 * 24 * 3_600_000;
const LIMIT: usize = 20;
const CLAUDE_TAIL_BYTES: u64 = 256 * 1024;
const CODEX_TAIL_BYTES: u64 = 64 * 1024;
const INTERRUPTED: &str = "[Request interrupted by user";

/// Sessions under `user_home` active in the last 3 days, newest first per tool.
pub fn read(user_home: &Path, now_ms: UnixMs) -> Vec<SessionMeta> {
    let since = now_ms - MAX_AGE_MS;
    let mut out = claude_sessions(&user_home.join(".claude/projects"), since);
    out.extend(codex_sessions(
        &user_home.join(".codex/session_index.jsonl"),
        since,
    ));
    out
}

fn claude_sessions(projects: &Path, since_ms: UnixMs) -> Vec<SessionMeta> {
    let mut files: Vec<(UnixMs, PathBuf)> = std::fs::read_dir(projects)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|project| std::fs::read_dir(project.path()).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| modified_ms(&p).map(|t| (t, p)))
        .filter(|(t, _)| *t >= since_ms)
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files
        .into_iter()
        .take(LIMIT)
        .filter_map(|(at, path)| {
            let tail = read_tail(&path, CLAUDE_TAIL_BYTES)
                .map_err(|e| tracing::debug!(file = %path.display(), "session unreadable: {e}"))
                .ok()?;
            let id = path.file_stem()?.to_string_lossy().into_owned();
            Some(SessionMeta {
                tool: "claude".into(),
                id,
                last_activity_ms: at,
                ..scan_claude(&tail)
            })
        })
        .collect()
}

fn modified_ms(path: &Path) -> Option<UnixMs> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    i64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The last `max` bytes, starting at the next full line.
fn read_tail(path: &Path, max: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok(match (start, text.find('\n')) {
        (0, _) | (_, None) => text,
        (_, Some(i)) => text[i + 1..].to_string(),
    })
}

/// Title, project and signals from the tail of a Claude Code transcript.
pub fn scan_claude(tail: &str) -> SessionMeta {
    let (mut custom, mut ai_title, mut project) = (None, None, None);
    let (mut failing, mut todos_open, mut interrupted) = (false, false, false);
    for rec in tail
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let s = |k: &str| rec.get(k).and_then(Value::as_str).map(str::to_string);
        if let Some(cwd) = s("cwd") {
            project = Path::new(&cwd)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
        }
        let content = rec.pointer("/message/content");
        match s("type").as_deref() {
            Some("custom-title") => custom = s("customTitle"),
            Some("ai-title") => ai_title = s("aiTitle"),
            Some("assistant") => {
                interrupted = false;
                todos_open = latest_todos(content).unwrap_or(todos_open);
            }
            Some("user") => {
                interrupted = texts(content, "text").any(|t| t.contains(INTERRUPTED))
                    || content
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.contains(INTERRUPTED));
                for out in texts(content, "tool_result") {
                    failing = test_outcome(&out).unwrap_or(failing);
                }
            }
            _ => {}
        }
    }
    let signals = [
        (failing, SessionSignal::TestsFailing),
        (todos_open, SessionSignal::TodosOpen),
        (interrupted, SessionSignal::Interrupted),
    ];
    SessionMeta {
        title: custom.or(ai_title),
        project,
        signals: signals
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, s)| *s)
            .collect(),
        ..Default::default()
    }
}

/// Text of content blocks of one type; tool results may hold a string or text blocks.
fn texts<'a>(content: Option<&'a Value>, kind: &'a str) -> impl Iterator<Item = String> + 'a {
    let blocks = content.and_then(Value::as_array).into_iter().flatten();
    blocks
        .filter(move |b| b.get("type").and_then(Value::as_str) == Some(kind))
        .map(move |b| {
            let inner = if kind == "text" {
                b.get("text")
            } else {
                b.get("content")
            };
            match inner {
                Some(Value::String(t)) => t.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect(),
                _ => String::new(),
            }
        })
}

/// Some(open) when this assistant message carries a TodoWrite call.
fn latest_todos(content: Option<&Value>) -> Option<bool> {
    let blocks = content.and_then(Value::as_array)?;
    let call = blocks.iter().rev().find(|b| {
        b.get("type").and_then(Value::as_str) == Some("tool_use")
            && b.get("name").and_then(Value::as_str) == Some("TodoWrite")
    })?;
    let todos = call.pointer("/input/todos").and_then(Value::as_array)?;
    Some(
        todos
            .iter()
            .any(|t| t.get("status").and_then(Value::as_str) != Some("completed")),
    )
}

/// Some(true) for a failed test run, Some(false) for a passing one, None otherwise.
fn test_outcome(out: &str) -> Option<bool> {
    let suite = out.contains("Tests:") || out.contains("Test Files") || out.contains(" passed");
    if out.contains("test result: FAILED") || out.contains("FAILURES!") || nonzero_failed(out) {
        Some(true)
    } else if out.contains("test result: ok") || (suite && out.contains(" passed")) {
        Some(false)
    } else {
        None
    }
}

/// "1 failed" yes, "0 failed" no.
fn nonzero_failed(out: &str) -> bool {
    out.match_indices(" failed").any(|(i, _)| {
        let digits = out[..i]
            .bytes()
            .rev()
            .take_while(u8::is_ascii_digit)
            .count();
        out[i - digits..i].parse::<u64>().is_ok_and(|n| n > 0)
    })
}

/// Codex keeps thread names in `session_index.jsonl`; later lines win.
fn codex_sessions(index: &Path, since_ms: UnixMs) -> Vec<SessionMeta> {
    let Ok(tail) = read_tail(index, CODEX_TAIL_BYTES) else {
        return Vec::new();
    };
    let mut by_id: std::collections::BTreeMap<String, SessionMeta> = Default::default();
    for rec in tail
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        let s = |k: &str| rec.get(k).and_then(Value::as_str);
        let (Some(id), Some(at)) = (s("id"), s("updated_at")) else {
            continue;
        };
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(at) else {
            continue;
        };
        let meta = SessionMeta {
            tool: "codex".into(),
            id: id.into(),
            title: s("thread_name").map(str::to_string),
            last_activity_ms: at.timestamp_millis(),
            ..Default::default()
        };
        by_id.insert(id.into(), meta);
    }
    let mut out: Vec<SessionMeta> = by_id
        .into_values()
        .filter(|m| m.last_activity_ms >= since_ms)
        .collect();
    out.sort_by_key(|m| std::cmp::Reverse(m.last_activity_ms));
    out.truncate(LIMIT);
    out
}
