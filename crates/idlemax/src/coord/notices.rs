//! The notice step of the coordinator sweep (adapter): gathers the finished runs and plain
//! jobs, the "needs you" lines of the dispatcher view and the day's facts, asks the pure
//! rules of `notify::notice` what is worth a notice, and stores each one with its keys.

use idlemax_core::notify::notice::{
    daily_summary, finished_batch, job_item, needs_batch, run_item, summary_key, summary_window,
    DayFacts, Item, NEEDS_REARM_MS,
};
use idlemax_core::quota::owner_use::DAY_MS;
use idlemax_core::types::UnixMs;
use idlemax_core::worker::run::RunState;
use idlemax_core::Config;

use super::api::gather_view;
use super::store::{Store, StoreError};

pub fn sweep(
    store: &mut Store,
    cfg: &Config,
    live: &Config,
    now: UnixMs,
    utc_offset_secs: i32,
) -> Result<(), StoreError> {
    finished(store, now)?;
    needs(store, cfg, live, now)?;
    daily(store, live, now, utc_offset_secs)
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
    let mut waiting = 0;
    for state in [RunState::Clean, RunState::NeedsYou] {
        waiting += store.list_runs(Some(state), None, 0)?.len();
    }
    let facts = DayFacts {
        tasks_done: store.tasks_done_between(start, end)?,
        accounts,
        waiting: u32::try_from(waiting).unwrap_or(u32::MAX),
    };
    if let Some(draft) = daily_summary(&facts, now, offset) {
        store.insert_notice(&draft, now)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use idlemax_core::dispatch::room::{LineState, RoomDecision};
    use idlemax_core::notify::notice::NoticeKind;
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::worker::run::{Run, RunLaunch, Verdict};

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
        // UTC 23:00 or so: no daily summary in this test.
        let sweep_at = |store: &mut Store, at| sweep(store, &cfg, &cfg, at, 0).unwrap();
        beat(&mut store, LineState::Run, T0);
        sweep_at(&mut store, T0 + 11 * MIN);
        sweep_at(&mut store, T0 + 12 * MIN);
        assert_eq!(kinds(&store), [NoticeKind::TaskFinished]);

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
}
