//! Domain: the three kinds of notice that interrupt the owner (D17, A22): a finished task, a
//! thing that needs the owner, and one daily summary. Everything else gives no notice. Pure;
//! the coordinator sweep stores the drafts and covers their keys.

use chrono::{DateTime, Duration, FixedOffset, NaiveTime, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

use crate::advice::ai::is_ai_pass;
use crate::dispatch::label::{one_line, run_label, run_summary, RunLine};
use crate::dispatch::view::NeedsYouLine;
use crate::notify::job_result_label;
use crate::types::{Job, JobSpec, JobStatus, UnixMs};
use crate::worker::run::{Run, Verdict};

/// Finished tasks within this long of the first one share a message, and none waits longer.
pub const FINISH_BATCH_MS: i64 = 600_000;
/// A "needs you" key gone from the view this long notifies again when it returns.
pub const NEEDS_REARM_MS: i64 = 3_600_000;
/// The daily summary goes out after 09:00 in the coordinator's local time (A22).
pub const SUMMARY_HOUR: u32 = 9;
const MAX_NEEDS_LINES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    #[default]
    TaskFinished,
    NeedsYou,
    DailySummary,
}

impl NoticeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NoticeKind::TaskFinished => "task_finished",
            NoticeKind::NeedsYou => "needs_you",
            NoticeKind::DailySummary => "daily_summary",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Notice {
    pub id: i64,
    pub at_ms: UnixMs,
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
}

/// A notice to store, with the keys it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub kind: NoticeKind,
    pub title: String,
    pub body: String,
    pub keys: Vec<String>,
}

/// One finished task waiting for its notice: `name` is the project or the job name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub key: String,
    pub at_ms: UnixMs,
    pub name: String,
    pub text: String,
}

/// A run that finished with the verdict `clean` or `needs_you`; a rejected run is never
/// reported as done (D8), and a run still going is nothing.
pub fn run_item(run: &Run) -> Option<Item> {
    if !matches!(run.verdict, Some(Verdict::Clean | Verdict::NeedsYou)) {
        return None;
    }
    let line = RunLine {
        run: run.clone(),
        frozen: false,
    };
    // One line per item: a batch body and a desktop notification have no room for more.
    let done = run
        .summary
        .as_deref()
        .map_or_else(|| run_summary(&line), one_line);
    Some(Item {
        key: format!("run:{}", run.id),
        at_ms: run.end_ms.unwrap_or(run.updated_ms),
        name: run.project.clone(),
        text: format!("{done} - {}", run_label(run.state, false)),
    })
}

/// A plain job that ended `succeeded`, `failed`, `killed` or `parked` (A22).
pub fn job_item(job: &Job) -> Option<Item> {
    let ended = matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Killed | JobStatus::Parked
    );
    if !ended || !is_plain_job(&job.spec) {
        return None;
    }
    Some(Item {
        key: format!("job:{}", job.id),
        at_ms: job.finished_ms.unwrap_or(job.created_ms),
        name: job.spec.name.clone(),
        text: job_result_label(job.status, job.exit_code, job.last_error.as_deref()),
    })
}

/// Not a worker, not the advisor's pass and not a job the dispatcher queued (`dispatch-`).
pub fn is_plain_job(spec: &JobSpec) -> bool {
    spec.worker.is_none() && !is_ai_pass(spec) && !spec.name.starts_with("dispatch-")
}

/// Nothing until the oldest waiting item finished `FINISH_BATCH_MS` ago, then one draft for
/// every item that finished within that window of it.
pub fn finished_batch(waiting: &[Item], now: UnixMs) -> Option<Draft> {
    let oldest = waiting.iter().map(|i| i.at_ms).min()?;
    if now - oldest < FINISH_BATCH_MS {
        return None;
    }
    let mut batch: Vec<&Item> = waiting
        .iter()
        .filter(|i| i.at_ms <= oldest + FINISH_BATCH_MS)
        .collect();
    batch.sort_by_key(|i| (i.at_ms, i.key.clone()));
    let (title, body) = match batch.as_slice() {
        [one] => (format!("idlemax: {}", one.name), one.text.clone()),
        many => (
            format!("idlemax: {} tasks finished", many.len()),
            many.iter()
                .map(|i| format!("{}: {}", i.name, i.text))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    };
    Some(Draft {
        kind: NoticeKind::TaskFinished,
        title,
        body,
        keys: batch.iter().map(|i| i.key.clone()).collect(),
    })
}

/// One draft for every "needs you" line no notice covers yet.
pub fn needs_batch(new_lines: &[NeedsYouLine]) -> Option<Draft> {
    let (title, body) = match new_lines {
        [] => return None,
        [one] => (
            format!("idlemax needs you: {}", one.subject),
            one.reason.clone(),
        ),
        many => {
            let mut lines: Vec<String> = many
                .iter()
                .take(MAX_NEEDS_LINES)
                .map(|l| format!("{}: {}", l.subject, l.reason))
                .collect();
            if many.len() > MAX_NEEDS_LINES {
                let more = many.len() - MAX_NEEDS_LINES;
                lines.push(format!("and {more} more: idlemax status"));
            }
            (
                format!("idlemax needs you: {} things", many.len()),
                lines.join("\n"),
            )
        }
    };
    Some(Draft {
        kind: NoticeKind::NeedsYou,
        title,
        body,
        keys: new_lines.iter().map(|l| l.key.clone()).collect(),
    })
}

/// What one day held, over the window of `summary_window`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DayFacts {
    /// Runs that reached `clean` or `needs_you`.
    pub tasks_done: u32,
    /// Per account its last closed account day: (account, idlemax's spend, the spendable
    /// percent when the day stored one).
    pub accounts: Vec<(String, f64, Option<f64>)>,
    pub waiting: u32,
}

/// The 24 hours that ended at today's `SUMMARY_HOUR`, once local time has passed it.
pub fn summary_window(now: UnixMs, utc_offset_secs: i32) -> Option<(UnixMs, UnixMs)> {
    let local = local(now, utc_offset_secs)?;
    if local.hour() < SUMMARY_HOUR {
        return None;
    }
    let at = NaiveTime::from_hms_opt(SUMMARY_HOUR, 0, 0)?;
    let end = local.date_naive().and_time(at);
    let end = local.timezone().from_local_datetime(&end).single()?;
    let end_ms = end.timestamp_millis();
    Some((end_ms - Duration::days(1).num_milliseconds(), end_ms))
}

/// The daily summary, keyed by the local date; nothing before 09:00 or for an empty day.
pub fn daily_summary(facts: &DayFacts, now: UnixMs, utc_offset_secs: i32) -> Option<Draft> {
    summary_window(now, utc_offset_secs)?;
    let spent = facts.accounts.iter().any(|(_, pct, _)| *pct > 0.0);
    if facts.tasks_done == 0 && !spent && facts.waiting == 0 {
        return None;
    }
    let mut lines = vec![format!("Tasks done: {}", facts.tasks_done)];
    for (account, pct, budget) in &facts.accounts {
        let mut line = format!("{account}: spent {pct:.0}%");
        if let Some(b) = budget {
            line.push_str(&format!(" of {b:.0}% budget"));
        }
        lines.push(line);
    }
    lines.push(format!("Waiting for review: {}", facts.waiting));
    Some(Draft {
        kind: NoticeKind::DailySummary,
        title: "idlemax: daily summary".into(),
        body: lines.join("\n"),
        keys: vec![summary_key(now, utc_offset_secs)?],
    })
}

/// The key of today's summary, `daily:<local date>`, once local time has passed 09:00.
pub fn summary_key(now: UnixMs, utc_offset_secs: i32) -> Option<String> {
    summary_window(now, utc_offset_secs)?;
    Some(format!(
        "daily:{}",
        local(now, utc_offset_secs)?.date_naive()
    ))
}

fn local(ms: UnixMs, utc_offset_secs: i32) -> Option<DateTime<FixedOffset>> {
    let tz = FixedOffset::east_opt(utc_offset_secs)?;
    tz.timestamp_millis_opt(ms).single()
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
