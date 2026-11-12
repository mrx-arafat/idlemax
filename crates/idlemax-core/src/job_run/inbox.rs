//! Domain: inbox notes for a finished job and for a notice (file name and markdown body),
//! and the parse of either for listings. Pure.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::job_run::Outcome;
use crate::notify::notice::Notice;
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

/// `<local date>-n<id>-<kind>.md`.
pub fn notice_file_name(n: &Notice, utc_offset_secs: i32) -> String {
    let date = local(n.at_ms, utc_offset_secs).map_or_else(
        || "undated".to_string(),
        |t| t.format("%Y-%m-%d").to_string(),
    );
    format!("{date}-n{}-{}.md", n.id, n.kind.as_str())
}

pub fn render_notice(n: &Notice, utc_offset_secs: i32) -> String {
    format!(
        "# {}\n\n- Notice: {} ({})\n- At: {}\n\n{}\n",
        n.title,
        n.id,
        n.kind.as_str(),
        stamp(n.at_ms, utc_offset_secs),
        n.body
    )
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

/// Summary of a finished job note for inbox listings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntry {
    pub time: String,
    /// Set for a job note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    /// Set for a notice note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<i64>,
    pub name: String,
    pub result: String,
    pub file_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<UnixMs>,
}

impl InboxEntry {
    /// How `idlemax inbox show` names the note: the job id, or `n<id>` for a notice.
    pub fn reference(&self) -> String {
        match (self.job_id, self.notice) {
            (Some(job), _) => job.to_string(),
            (None, Some(n)) => format!("n{n}"),
            (None, None) => "-".into(),
        }
    }
}

/// Parses an inbox note's markdown body and filename into an [`InboxEntry`].
pub fn parse_note(file_name: &str, content: &str) -> Option<InboxEntry> {
    parse_notice(file_name, content).or_else(|| parse_job_note(file_name, content))
}

/// A note with a `- Notice: <id> (<kind>)` line: the title is the notice's own.
fn parse_notice(file_name: &str, content: &str) -> Option<InboxEntry> {
    let line = |prefix: &str| {
        content
            .lines()
            .find_map(|l| l.trim().strip_prefix(prefix).map(str::trim))
    };
    let (id, kind) = line("- Notice: ")?.split_once(' ')?;
    let time = line("- At: ").unwrap_or("unknown").to_string();
    Some(InboxEntry {
        finished_ms: DateTime::parse_from_str(&time, "%Y-%m-%d %H:%M:%S %:z")
            .ok()
            .map(|dt| dt.timestamp_millis()),
        time,
        job_id: None,
        notice: Some(id.parse().ok()?),
        name: line("# ").unwrap_or_default().to_string(),
        result: kind.trim_matches(|c| c == '(' || c == ')').to_string(),
        file_name: file_name.to_string(),
    })
}

fn parse_job_note(file_name: &str, content: &str) -> Option<InboxEntry> {
    let mut title_name = None;
    let mut title_status = None;
    let mut job_id = None;
    let mut finished_str = None;
    let mut started_str = None;
    let mut exit_code = None;
    let mut error = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && title_name.is_none() {
            let rest = &trimmed[2..];
            if let Some((name, status)) = rest.split_once(": ") {
                title_name = Some(name.trim().to_string());
                title_status = Some(status.trim().to_string());
            } else {
                title_name = Some(rest.trim().to_string());
            }
        } else if let Some(rest) = trimmed.strip_prefix("- Job: ") {
            let id_part = rest.split(|c: char| c.is_whitespace() || c == '(').next();
            if let Some(id_str) = id_part {
                if let Ok(id) = id_str.parse::<JobId>() {
                    job_id = Some(id);
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("- Finished: ") {
            finished_str = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("- Started: ") {
            started_str = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("- Exit code: ") {
            exit_code = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("- Error: ") {
            error = Some(rest.trim().to_string());
        }
    }

    let job_id = job_id.or_else(|| parse_id_from_filename(file_name))?;
    let name = title_name.unwrap_or_else(|| format!("job {job_id}"));
    let status_raw = title_status.unwrap_or_default();

    let result = if let Some(err) = error {
        if !err.is_empty() {
            if !status_raw.is_empty() {
                format!("{status_raw}: {err}")
            } else {
                err
            }
        } else {
            status_raw
        }
    } else if let Some(ref code) = exit_code {
        if code != "0" && code != "none" && !status_raw.is_empty() {
            format!("{status_raw} (exit {code})")
        } else if !status_raw.is_empty() {
            status_raw
        } else {
            format!("exit {code}")
        }
    } else if !status_raw.is_empty() {
        status_raw
    } else {
        "unknown".to_string()
    };

    let time = finished_str
        .or(started_str)
        .unwrap_or_else(|| parse_date_from_filename(file_name).unwrap_or_else(|| "unknown".into()));

    let finished_ms = DateTime::parse_from_str(&time, "%Y-%m-%d %H:%M:%S %:z")
        .ok()
        .map(|dt| dt.timestamp_millis());

    Some(InboxEntry {
        time,
        job_id: Some(job_id),
        notice: None,
        name,
        result,
        file_name: file_name.to_string(),
        finished_ms,
    })
}

/// Tries to extract job ID from `<date>-<id>-<name>.md`.
fn parse_id_from_filename(name: &str) -> Option<JobId> {
    let clean = name.strip_suffix(".md").unwrap_or(name);
    let parts: Vec<&str> = clean.split('-').collect();
    if parts.len() >= 4 {
        parts[3].parse().ok()
    } else {
        None
    }
}

/// Tries to extract `YYYY-MM-DD` from `<date>-<id>-<name>.md`.
fn parse_date_from_filename(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() >= 3 && parts[0].len() == 4 && parts[1].len() == 2 && parts[2].len() == 2 {
        Some(format!("{}-{}-{}", parts[0], parts[1], parts[2]))
    } else {
        None
    }
}

/// Sorts inbox entries newest first (descending finished timestamp, then descending ID).
pub fn sort_entries(entries: &mut [InboxEntry]) {
    entries.sort_by(|a, b| {
        b.finished_ms
            .cmp(&a.finished_ms)
            .then_with(|| b.time.cmp(&a.time))
            .then_with(|| b.job_id.cmp(&a.job_id))
            .then_with(|| b.notice.cmp(&a.notice))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CostClass, JobSpec, JobStatus};

    #[test]
    fn render_and_parse_roundtrip() {
        let job = Job {
            id: 42,
            spec: JobSpec {
                name: "echo test".into(),
                command: vec!["/bin/echo".into(), "hello".into()],
                cost: CostClass::CpuLight,
                ..JobSpec::default()
            },
            attempts: 1,
            ..Job::default()
        };
        let outcome = Outcome {
            status: JobStatus::Succeeded,
            exit_code: Some(0),
            error: None,
        };
        let report = RunReport {
            job: &job,
            device: "laptop",
            outcome: &outcome,
            started_ms: 1730000000000,
            finished_ms: 1730000001000,
            active_ms: 1000,
            wrapper: "taskpolicy",
            log_path: "/tmp/42.log",
            output: "hello\n",
            utc_offset_secs: 0,
        };
        let rendered = render(&report);
        let fname = file_name(job.id, &job.spec.name, report.finished_ms, 0);
        let entry = parse_note(&fname, &rendered).expect("parse note");

        assert_eq!(entry.job_id, Some(42));
        assert_eq!(entry.name, "echo test");
        assert_eq!(entry.result, "succeeded");
        assert_eq!(entry.file_name, fname);
        assert!(entry.finished_ms.is_some());
    }

    #[test]
    fn parse_failed_with_error() {
        let note = "# compile: failed\n\n- Job: 7 (attempt 2)\n- Exit code: 1\n- Error: out of memory\n- Finished: 2026-11-08 10:00:00 +00:00\n";
        let entry = parse_note("2026-11-08-7-compile.md", note).expect("parse note");
        assert_eq!(entry.job_id, Some(7));
        assert_eq!(entry.name, "compile");
        assert_eq!(entry.result, "failed: out of memory");
        assert_eq!(entry.time, "2026-11-08 10:00:00 +00:00");
    }

    #[test]
    fn sort_newest_first() {
        let mut entries = vec![
            InboxEntry {
                time: "2026-11-08 09:00:00 +00:00".into(),
                job_id: Some(1),
                notice: None,
                name: "first".into(),
                result: "succeeded".into(),
                file_name: "f1.md".into(),
                finished_ms: Some(100),
            },
            InboxEntry {
                time: "2026-11-08 11:00:00 +00:00".into(),
                job_id: Some(3),
                notice: None,
                name: "third".into(),
                result: "succeeded".into(),
                file_name: "f3.md".into(),
                finished_ms: Some(300),
            },
            InboxEntry {
                time: "2026-11-08 10:00:00 +00:00".into(),
                job_id: Some(2),
                notice: None,
                name: "second".into(),
                result: "succeeded".into(),
                file_name: "f2.md".into(),
                finished_ms: Some(200),
            },
        ];
        sort_entries(&mut entries);
        assert_eq!(entries[0].job_id, Some(3));
        assert_eq!(entries[1].job_id, Some(2));
        assert_eq!(entries[2].job_id, Some(1));
    }

    #[test]
    fn a_notice_note_renders_and_parses_back_and_a_job_note_keeps_its_keys() {
        use crate::notify::notice::{Notice, NoticeKind};
        let n = Notice {
            id: 12,
            at_ms: 1_800_000_000_000,
            kind: NoticeKind::TaskFinished,
            title: "idlemax: app".into(),
            body: "Fix the login test: checks passed - ready for review".into(),
        };
        let name = notice_file_name(&n, 0);
        assert_eq!(name, "2027-01-15-n12-task_finished.md");
        let text = render_notice(&n, 0);
        assert!(text.ends_with("ready for review\n"), "{text}");
        let entry = parse_note(&name, &text).unwrap();
        assert_eq!((entry.notice, entry.job_id), (Some(12), None));
        assert_eq!(entry.name, "idlemax: app");
        assert_eq!(entry.result, "task_finished");
        assert_eq!(entry.finished_ms, Some(n.at_ms));

        let note =
            "# compile: failed\n\n- Job: 7 (attempt 2)\n- Finished: 2026-11-08 10:00:00 +00:00\n";
        let job = parse_note("2026-11-08-7-compile.md", note).unwrap();
        let keys: Vec<String> = serde_json::to_value(&job)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys,
            [
                "file_name",
                "finished_ms",
                "job_id",
                "name",
                "result",
                "time"
            ]
        );
    }
}
