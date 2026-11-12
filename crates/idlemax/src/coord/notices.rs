//! The notice step of the coordinator sweep (adapter): gathers the finished runs and plain
//! jobs, the "needs you" lines of the dispatcher view and the day's facts, asks the pure
//! rules of `notify::notice` what is worth a notice, and stores each one with its keys.

use idlemax_core::notify::notice::{
    daily_summary, finished_batch, job_item, needs_batch, run_item, summary_key, summary_window,
    DayFacts, Item, NEEDS_REARM_MS,
};
use idlemax_core::quota::owner_use::DAY_MS;
use idlemax_core::types::UnixMs;
use idlemax_core::Config;

use super::api::gather_view;
use super::store::{Store, StoreError};

/// Runs the three steps; each logs its own failure, so a failing view (the whole plan,
/// every 15 s) never holds back finished tasks or the daily summary.
pub fn sweep(store: &mut Store, cfg: &Config, live: &Config, now: UnixMs, utc_offset_secs: i32) {
    if let Err(e) = finished(store, now) {
        tracing::warn!(error = %e, "finished-task notices failed");
    }
    if let Err(e) = needs(store, cfg, live, now) {
        tracing::warn!(error = %e, "needs-you notices failed");
    }
    if let Err(e) = daily(store, live, now, utc_offset_secs) {
        tracing::warn!(error = %e, "daily summary failed");
    }
}

fn finished(store: &mut Store, now: UnixMs) -> Result<(), StoreError> {
    let since = now - DAY_MS;
    let mut items: Vec<Item> = store
        .unnoticed_runs_since(since)?
        .iter()
        .filter_map(run_item)
        .collect();
    items.extend(
        store
            .finished_jobs_since(since)?
            .iter()
            .filter_map(job_item),
    );
    if let Some(draft) = finished_batch(&items, now) {
        store.insert_notice(&draft, now)?;
    }
    Ok(())
}

fn needs(store: &mut Store, cfg: &Config, live: &Config, now: UnixMs) -> Result<(), StoreError> {
    let view = gather_view(store, cfg, live, now)?;
    let mut seen = Vec::new();
    let mut new = Vec::new();
    for line in view.needs_you {
        if store.is_covered(&line.key)? {
            seen.push(line.key);
        } else {
            new.push(line);
        }
    }
    store.refresh_needs(&seen, now, now - NEEDS_REARM_MS)?;
    if let Some(draft) = needs_batch(&new) {
        store.insert_notice(&draft, now)?;
    }
    Ok(())
}

fn daily(store: &mut Store, live: &Config, now: UnixMs, offset: i32) -> Result<(), StoreError> {
    let Some((start, end)) = summary_window(now, offset) else {
        return Ok(());
    };
    let Some(key) = summary_key(now, offset) else {
        return Ok(());
    };
    if store.is_covered(&key)? {
        return Ok(());
    }
    let mut accounts = Vec::new();
    for a in &live.quota.accounts {
        let last = store
            .budget_days(&a.name, end - 8 * DAY_MS)?
            .into_iter()
            .filter(|d| d.closed && d.day_start_ms < end)
            .max_by_key(|d| d.day_start_ms);
        if let Some(d) = last {
            accounts.push((
                a.name.clone(),
                d.idlemax_pct.unwrap_or(0.0),
                d.spendable_pct,
            ));
        }
    }
    let facts = DayFacts {
        tasks_done: store.tasks_done_between(start, end)?,
        accounts,
        waiting: store.waiting_runs()?,
    };
    // The day is checked once: an empty day sends nothing later either, and the facts are
    // not read again every 15 s until midnight.
    match daily_summary(&facts, now, offset) {
        Some(draft) => store.insert_notice(&draft, now).map(|_| ()),
        None => store.cover_key(&key, now),
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::dispatch::room::{LineState, RoomDecision};
    use idlemax_core::notify::notice::NoticeKind;
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::worker::run::{Run, RunLaunch, RunState, Verdict};

    use super::*;

    const T0: UnixMs = 1_800_000_000_000;
    const MIN: UnixMs = 60_000;

    fn beat(store: &mut Store, state: LineState, at: UnixMs) {
        let mut hb = Heartbeat::default();
        hb.device.id = "mac".into();
        hb.room = Some(RoomDecision {
            state,
            reason: "memory 97%".into(),
            ..RoomDecision::default()
        });
        store.record_heartbeat(&hb, at).unwrap();
    }

    fn kinds(store: &Store) -> Vec<NoticeKind> {
        let all = store.notices_after(0, 0, 0).unwrap();
        all.iter().map(|n| n.kind).collect()
    }

    #[test]
    fn a_finished_run_is_told_once_and_an_emergency_again_only_after_an_hour_away() {
        let mut store = Store::open_in_memory().unwrap();
        let cfg = Config::default();
        let run = Run {
            project: "app".into(),
            state: RunState::Clean,
            verdict: Some(Verdict::Clean),
            end_ms: Some(T0),
            updated_ms: T0,
            ..Run::default()
        };
        store.insert_run(&run, &RunLaunch::default()).unwrap();
        // A plain job a minute later shares the run's message.
        store
            .load_sql(&format!(
                "INSERT INTO jobs (id, spec, name, status, priority, created_ms, finished_ms)
             VALUES (50, '{{\"name\":\"backup\",\"command\":[\"true\"]}}', 'backup',
               'succeeded', 0, {T0}, {})",
                T0 + MIN
            ))
            .unwrap();
        // T0 is 08:00 UTC: no daily summary before the needs-you part of this test.
        let sweep_at = |store: &mut Store, at| sweep(store, &cfg, &cfg, at, 0);
        beat(&mut store, LineState::Run, T0);
        sweep_at(&mut store, T0 + 11 * MIN);
        sweep_at(&mut store, T0 + 12 * MIN);
        assert_eq!(kinds(&store), [NoticeKind::TaskFinished]);
        let told = store.notices_after(0, 0, 0).unwrap();
        assert_eq!(told[0].title, "idlemax: 2 tasks finished");

        let mut at = T0 + 20 * MIN;
        beat(&mut store, LineState::Emergency, at);
        sweep_at(&mut store, at);
        for (gone, back) in [(30, 1), (61, 2)] {
            at += MIN;
            beat(&mut store, LineState::Run, at);
            sweep_at(&mut store, at);
            at += gone * MIN;
            beat(&mut store, LineState::Run, at);
            sweep_at(&mut store, at);
            beat(&mut store, LineState::Emergency, at);
            sweep_at(&mut store, at);
            let needs = kinds(&store)
                .into_iter()
                .filter(|k| *k == NoticeKind::NeedsYou)
                .count();
            assert_eq!(needs, back, "gone {gone} minutes");
        }
    }

    #[test]
    fn the_daily_summary_goes_once_after_nine_local_and_an_empty_day_stays_quiet() {
        let cfg = Config::default();
        // T0 is 08:00 UTC, so 09:00 at UTC+1.
        let offset = 3600;
        let clean = Run {
            project: "app".into(),
            state: RunState::Clean,
            verdict: Some(Verdict::Clean),
            end_ms: Some(T0 - 60 * MIN),
            updated_ms: T0 - 60 * MIN,
            ..Run::default()
        };
        let mut store = Store::open_in_memory().unwrap();
        store.insert_run(&clean, &RunLaunch::default()).unwrap();
        let daily = |store: &Store| {
            kinds(store)
                .into_iter()
                .filter(|k| *k == NoticeKind::DailySummary)
                .count()
        };
        sweep(&mut store, &cfg, &cfg, T0 - MIN, offset);
        assert_eq!(daily(&store), 0);
        sweep(&mut store, &cfg, &cfg, T0, offset);
        sweep(&mut store, &cfg, &cfg, T0 + MIN, offset);
        assert_eq!(daily(&store), 1);

        let mut empty = Store::open_in_memory().unwrap();
        sweep(&mut empty, &cfg, &cfg, T0, offset);
        empty.insert_run(&clean, &RunLaunch::default()).unwrap();
        sweep(&mut empty, &cfg, &cfg, T0 + 300 * MIN, offset);
        assert_eq!(daily(&empty), 0);
    }
}
