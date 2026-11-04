//! Domain: inbox note for a finished job (file name and markdown body). Pure.

use chrono::{DateTime, FixedOffset};

use crate::job_run::Outcome;
use crate::types::{Job, JobId, UnixMs};

/// Output lines shown in a note.
pub const NOTE_TAIL_LINES: usize = 200;
const SLUG_MAX: usize = 48;

/// Everything a note shows about one finished attempt.
#[derive(Debug, Clone)]
pub struct RunReport<'a> {
    pub job: &'a Job,
    pub device: &'a str,
    pub outcome: &'a Outcome,
    pub started_ms: UnixMs,
    pub finished_ms: UnixMs,
    /// Run time without paused spans.
    pub active_ms: i64,
    pub wrapper: &'a str,
    pub log_path: &'a str,
    /// This attempt's output (stdout and stderr), possibly already truncated.
    pub output: &'a str,
    /// Device time zone, seconds east of UTC.
    pub utc_offset_secs: i32,
}

/// `<local date>-<id>-<name slug>.md`.
pub fn file_name(id: JobId, name: &str, finished_ms: UnixMs, utc_offset_secs: i32) -> String {
    let date = local(finished_ms, utc_offset_secs).map_or_else(
        || "undated".to_string(),
        |t| t.format("%Y-%m-%d").to_string(),
    );
    format!("{date}-{id}-{}.md", slug(name))
}

pub fn render(r: &RunReport<'_>) -> String {
    let job = r.job;
    let exit = r
        .outcome
        .exit_code
        .map_or_else(|| "none".to_string(), |c| c.to_string());
    let mut lines = vec![
        format!("# {}: {}", job.spec.name, r.outcome.status),
        String::new(),
        format!("- Job: {} (attempt {})", job.id, job.attempts),
        format!("- Device: {}", r.device),
        format!("- Started: {}", stamp(r.started_ms, r.utc_offset_secs)),
        format!("- Finished: {}", stamp(r.finished_ms, r.utc_offset_secs)),
        format!(
            "- Run time: {} (paused time excluded)",
            duration(r.active_ms)
        ),
        format!("- Exit code: {exit}"),
    ];
    if let Some(err) = &r.outcome.error {
        lines.push(format!("- Error: {err}"));
    }
    lines.push(format!("- Throttle: {}", r.wrapper));
    lines.push(format!("- Log: {}", r.log_path));
    lines.push(String::new());
    lines.push("## Command".into());
    lines.push(String::new());
    lines.push(fenced(
        &serde_json::to_string(&job.spec.command).unwrap_or_default(),
    ));
    lines.push(String::new());
    let shown = tail(r.output, NOTE_TAIL_LINES);
    if shown.trim().is_empty() {
        lines.push("## Output".into());
        lines.push(String::new());
        lines.push("_No output._".into());
    } else {
        lines.push(format!("## Output (last {NOTE_TAIL_LINES} lines at most)"));
        lines.push(String::new());
        lines.push(fenced(shown));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// The last `max_lines` lines of `text`.
pub fn tail(text: &str, max_lines: usize) -> &str {
    let text = text.trim_end_matches('\n');
    match max_lines.checked_sub(1) {
        None => "",
        Some(n) => match text.rmatch_indices('\n').nth(n) {
            Some((i, _)) => &text[i + 1..],
            None => text,
        },
    }
}

/// A code block whose fence is longer than any backtick run inside it.
fn fenced(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}text\n{text}\n{fence}")
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.chars().take(SLUG_MAX).collect();
    match out.trim_end_matches('-') {
        "" => "job".to_string(),
        s => s.to_string(),
    }
}

fn local(ms: UnixMs, utc_offset_secs: i32) -> Option<DateTime<FixedOffset>> {
    let tz = FixedOffset::east_opt(utc_offset_secs)?;
    Some(DateTime::from_timestamp_millis(ms)?.with_timezone(&tz))
}

fn stamp(ms: UnixMs, utc_offset_secs: i32) -> String {
    local(ms, utc_offset_secs).map_or_else(
        || format!("{ms} ms"),
        |t| t.format("%Y-%m-%d %H:%M:%S %:z").to_string(),
    )
}

fn duration(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s:02}s"),
        _ => format!("{h}h {m:02}m {s:02}s"),
    }
}
