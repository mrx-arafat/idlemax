//! Domain: the one line that answers "did it help, and what stopped it?" (pure).

use super::timeline::MAX_STUCK;
use super::{GuardSummary, Report};
use crate::types::JobId;

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

/// A length of time for people: seconds under a minute, whole minutes under an hour, hours
/// with one decimal up to two days, days with one decimal above.
pub fn duration(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < MINUTE_MS {
        format!("{} s", (ms + 500) / 1000)
    } else if ms < HOUR_MS {
        format!("{} min", (ms + MINUTE_MS / 2) / MINUTE_MS)
    } else if ms < 2 * DAY_MS {
        format!("{:.1} h", ms as f64 / HOUR_MS as f64)
    } else {
        format!("{:.1} days", ms as f64 / DAY_MS as f64)
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `today: 2 jobs ran (14 job-min); guard held 6.1 h, mostly memory pressure (5.2 h); queue
/// was empty 1.3 h of idle time`, or the reason nothing ran when nothing did.
pub(super) fn line(r: &Report) -> String {
    if r.jobs.ran == 0 {
        return format!("0 jobs ran {}: {}", period(&r.label), why_nothing(r));
    }
    let mut parts = vec![ran_text(r)];
    parts.extend(stuck_text(&r.idle.stuck_jobs));
    parts.extend(guard_parts(r));
    parts.extend(quota_part(r));
    format!("{}: {}", r.label, parts.join("; "))
}

/// "AI quota: 3 windows expired, 41% unused on average", with the span the history covers
/// when the coordinator keeps less than the period.
fn quota_part(r: &Report) -> Option<String> {
    let q = &r.quota;
    if !q.configured || q.expired.is_empty() {
        return None;
    }
    let n = q.expired.len();
    let unused: f64 = q.expired.iter().map(|c| c.unused_pct).sum();
    let kept = if q.counted_since_ms > r.since_ms {
        format!(
            " (only the last {} is kept)",
            duration(r.until_ms - q.counted_since_ms)
        )
    } else {
        String::new()
    };
    Some(format!(
        "AI quota{kept}: {} expired, {:.0}% unused on average",
        plural(n as u64, "window", "windows"),
        unused / n as f64
    ))
}

/// What to say about queued jobs that no device can take as configured.
fn stuck_text(ids: &[JobId]) -> Option<String> {
    match ids {
        [] => None,
        [id] => Some(format!(
            "job {id} is queued and no device can run it, run `idlemax why {id}`"
        )),
        [first, ..] => {
            let more = if ids.len() == MAX_STUCK {
                " or more"
            } else {
                ""
            };
            Some(format!(
                "{}{more} queued jobs have no device that can run them, run `idlemax why {first}`",
                ids.len()
            ))
        }
    }
}

/// "today", "in the last 7 days": how a period reads after "ran".
fn period(label: &str) -> String {
    match label {
        "today" | "yesterday" => label.to_string(),
        l if l.starts_with("last ") => format!("in the {l}"),
        l => format!("in {l}"),
    }
}

fn ran_text(r: &Report) -> String {
    let j = &r.jobs;
    let minutes = (j.job_ms + MINUTE_MS / 2) / MINUTE_MS;
    let used = if minutes == 0 && j.job_ms > 0 {
        "<1 job-min".to_string()
    } else {
        format!("{minutes} job-min")
    };
    let mut notes = vec![used];
    for (n, what) in [(j.failed, "failed"), (j.killed, "killed")] {
        if n > 0 {
            notes.push(format!("{n} {what}"));
        }
    }
    format!(
        "{} ran ({})",
        plural(j.ran, "job", "jobs"),
        notes.join(", ")
    )
}

/// What the guard and the idle time did, for a period in which jobs ran.
fn guard_parts(r: &Report) -> Vec<String> {
    let (g, i) = (&r.guard, &r.idle);
    if g.tracked_ms == 0 {
        return vec!["no guard history yet".into()];
    }
    let mut parts = Vec::new();
    if g.held_ms >= MINUTE_MS {
        parts.push(format!(
            "guard held {}{}",
            duration(g.held_ms),
            main_reason(r)
        ));
    }
    if g.offline_ms >= MINUTE_MS {
        parts.push(format!("asleep or offline {}", duration(g.offline_ms)));
    }
    if i.queue_empty_ms >= MINUTE_MS {
        parts.push(format!(
            "queue was empty {} of idle time",
            duration(i.queue_empty_ms)
        ));
    }
    if i.work_waiting_ms >= MINUTE_MS {
        parts.push(format!(
            "work waited {} with room to run it",
            duration(i.work_waiting_ms)
        ));
    }
    if i.quota_waiting_ms >= MINUTE_MS {
        parts.push(format!(
            "AI work waited {} for quota",
            duration(i.quota_waiting_ms)
        ));
    }
    parts
}

/// ", mostly memory pressure (5.2 h)" when one reason covers at least half the held time,
/// ", most often ..." when the biggest does not, and ", all of it by ..." when every reason
/// named was there the whole time.
fn main_reason(r: &Report) -> String {
    let g = &r.guard;
    let always = covering(g);
    if !always.is_empty() {
        return format!(", all of it by {}", and_list(&always));
    }
    let Some(top) = g.held_by.first() else {
        return String::new();
    };
    let how = if top.ms * 2 >= g.held_ms {
        "mostly"
    } else {
        "most often"
    };
    format!(", {how} {} ({})", top.label, duration(top.ms))
}

/// The labels of the reasons present for the whole held time, longest first.
fn covering(g: &GuardSummary) -> Vec<&str> {
    g.held_by
        .iter()
        .take_while(|h| h.ms == g.held_ms)
        .map(|h| h.label.as_str())
        .collect()
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The biggest reason a period with no jobs had none: what held the machine, work that sat
/// in the queue, nothing to run, or no heartbeat.
fn why_nothing(r: &Report) -> String {
    let (g, i) = (&r.guard, &r.idle);
    if g.tracked_ms == 0 {
        return "no device has reported in this period".into();
    }
    if let Some(stuck) = stuck_text(&i.stuck_jobs) {
        return stuck;
    }
    let held = held_text(g);
    let causes = [
        (g.held_ms, held),
        (
            i.work_waiting_ms,
            format!(
                "work was queued but did not start for {}, run `idlemax why <id>`",
                duration(i.work_waiting_ms)
            ),
        ),
        (
            i.quota_waiting_ms,
            format!(
                "AI work waited {} for quota, run `idlemax quota`",
                duration(i.quota_waiting_ms)
            ),
        ),
        (i.queue_empty_ms, "the queue was empty".to_string()),
        (
            // One device being away does not explain a period in which another was awake.
            if g.tracked_ms > g.offline_ms {
                0
            } else {
                g.offline_ms
            },
            format!(
                "the device was asleep or offline {}",
                duration(g.offline_ms)
            ),
        ),
    ];
    let mut best: Option<&(i64, String)> = None;
    for cause in &causes {
        if cause.0 > best.map_or(0, |b| b.0) {
            best = Some(cause);
        }
    }
    best.map_or_else(|| "the queue was empty".into(), |b| b.1.clone())
}

/// "held 7.9 h by memory pressure"; when no reason covered the whole time, the biggest one.
fn held_text(g: &GuardSummary) -> String {
    let held = duration(g.held_ms);
    let always = covering(g);
    match (always.is_empty(), g.held_by.first()) {
        (false, _) => format!("held {held} by {}", and_list(&always)),
        (true, Some(top)) => format!(
            "held {held}, mostly by {} ({})",
            top.label,
            duration(top.ms)
        ),
        (true, None) => format!("held {held}"),
    }
}
