//! Delivery: `idlemax report`, what idlemax did with the machine and what stopped it.
//! It asks the coordinator for one report per period and prints the verdict line first.

mod render;

use anyhow::Result;
use chrono::{DateTime, Local, TimeZone};
use clap::Args;
use idlemax_core::report::{Report, MAX_DAYS};
use idlemax_core::types::UnixMs;

use super::client::Period;
use super::rows::Zone;
use super::{ApiClient, Out};

const DAY_MS: i64 = 86_400_000;

#[derive(Args, Debug)]
pub(super) struct ReportArgs {
    /// Report on the last N days only, 1 to 30 [default: today and the last 7 days]
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(1..=MAX_DAYS))]
    days: Option<i64>,
}

pub(super) async fn run(
    c: &ApiClient,
    args: ReportArgs,
    out: &Out,
    now: UnixMs,
    zone: Zone,
) -> Result<()> {
    let reports = match args.days {
        Some(n) => vec![c.report(Period::Days(n)).await?],
        None => {
            let [today, week] = default_periods(now, zone);
            let (today, week) = tokio::try_join!(c.report(today), c.report(week))?;
            vec![today, week]
        }
    };
    out.emit_with(
        &reports,
        |r| render::text(r, zone),
        || Ok(render::view(&reports, zone)),
        &hints(&reports),
    )
}

/// Today so far and the last 7 days: what `idlemax report` and the dashboards show.
pub(crate) fn default_periods(now: UnixMs, zone: Zone) -> [Period; 2] {
    let today = Period::Since {
        since_ms: start_of_day(now, zone),
        label: "today",
    };
    [today, Period::Days(7)]
}

/// Midnight at the start of `now`'s day in `zone`, always before `now`.
fn start_of_day(now: UnixMs, zone: Zone) -> UnixMs {
    let midnight = DateTime::from_timestamp_millis(now).and_then(|t| match zone {
        Zone::Utc => t
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|d| d.and_utc().timestamp_millis()),
        Zone::Local => {
            let day = t.with_timezone(&Local).date_naive().and_hms_opt(0, 0, 0)?;
            Some(
                Local
                    .from_local_datetime(&day)
                    .earliest()?
                    .timestamp_millis(),
            )
        }
    });
    midnight.unwrap_or(now - DAY_MS).min(now - 1)
}

/// Less than this of anything is noise, not a reason to run another command.
const WORTH_A_HINT_MS: i64 = 60_000;

/// What to run next, for the first report that shows a problem worth acting on.
fn hints(reports: &[Report]) -> Vec<String> {
    let mut hints = Vec::new();
    let any = |f: &dyn Fn(&Report) -> bool| reports.iter().any(f);
    if any(&|r| r.guard.tracked_ms == 0) {
        hints.push("Run `idlemax status` to see whether an agent is reporting".into());
    }
    if any(&|r| r.idle.work_waiting_ms >= WORTH_A_HINT_MS) {
        hints.push(
            "Run `idlemax jobs --status queued` to find the waiting job, then `idlemax why <id>`"
                .into(),
        );
    }
    if any(&|r| r.guard.held_ms >= WORTH_A_HINT_MS) {
        hints.push("Run `idlemax config` to see the guard rules you can change".into());
    }
    if any(&|r| r.jobs.ran == 0 && r.idle.queue_empty_ms >= WORTH_A_HINT_MS) {
        hints.push("Run `idlemax advise` to see what is worth queueing".into());
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_starts_at_midnight_and_never_at_or_after_now() {
        // 2024-10-27 03:33:20 UTC.
        let now = 1_730_000_000_000;
        assert_eq!(start_of_day(now, Zone::Utc), 1_729_987_200_000);
        let at_midnight = 1_729_987_200_000;
        assert_eq!(start_of_day(at_midnight, Zone::Utc), at_midnight - 1);
    }
}
