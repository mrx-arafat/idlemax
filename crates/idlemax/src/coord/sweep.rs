//! Coordinator background sweeps (adapter): lease expiry, offline devices and due schedules
//! every 15 s, store pruning hourly, the dispatcher step every minute, advisor refresh every
//! 15 min. Loops log failures and continue.

use std::time::Duration;

use idlemax_core::scheduler::expire_leases;
use idlemax_core::types::{EventKind, UnixMs};
use idlemax_core::Config;
use tokio::task::JoinHandle;
use tokio::time::{interval, interval_at, Instant, MissedTickBehavior};

use super::api::{offline_after_ms, AppState};
use super::schedule;
use super::store::{Store, StoreError};
use crate::state::now_ms;

#[derive(Debug, thiserror::Error)]
enum SweepError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("store task: {0}")]
    Join(#[from] tokio::task::JoinError),
}

const SWEEP_EVERY: Duration = Duration::from_secs(15);
const PRUNE_EVERY_MS: i64 = 3_600_000;
const ADVISE_EVERY: Duration = Duration::from_secs(15 * 60);
/// Lets the first heartbeats arrive before the first advisor run.
const ADVISE_FIRST_AFTER: Duration = Duration::from_secs(60);
const DAY_MS: i64 = 86_400_000;
/// How often the dispatcher moves each project's base check on.
const DISPATCH_EVERY: Duration = Duration::from_secs(60);

/// Starts the sweep loops; the caller aborts them at shutdown.
pub fn spawn(app: AppState) -> Vec<JoinHandle<()>> {
    // The dispatcher has its own switch (`dispatcher.mode`), so it never hangs on the advisor's.
    let mut tasks = vec![
        tokio::spawn(maintain(app.clone())),
        tokio::spawn(dispatch(app.clone())),
    ];
    if app.config().advisor.enabled {
        tasks.push(tokio::spawn(advise(app)));
    }
    tasks
}

async fn maintain(app: AppState) {
    let mut tick = interval(SWEEP_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_prune_ms = 0;
    loop {
        tick.tick().await;
        let now = now_ms();
        let prune = now - last_prune_ms >= PRUNE_EVERY_MS;
        match app
            .with_store(move |store, cfg| sweep_once(store, cfg, prune, now))
            .await
        {
            Ok(()) if prune => last_prune_ms = now,
            Ok(()) => {}
            Err(e) => tracing::warn!(error = %e, "coordinator sweep failed"),
        }
    }
}

async fn dispatch(app: AppState) {
    let mut tick = interval(DISPATCH_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let _serial = app.dispatch_lock().lock().await;
        let live = std::sync::Arc::clone(app.live());
        let paths = app.paths().clone();
        let now = now_ms();
        let done = app
            .with_store(move |store, cfg| {
                let rules = crate::dispatch::rules_file::read(&paths);
                crate::dispatch::step(store, cfg, &live.current(), &rules, now, false)
                    .map_err(SweepError::from)
            })
            .await;
        if let Err(e) = done {
            tracing::warn!(error = %e, "dispatcher step failed");
        }
    }
}

async fn advise(app: AppState) {
    let mut tick = interval_at(Instant::now() + ADVISE_FIRST_AFTER, ADVISE_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = super::api::run_refresh(&app, now_ms()).await {
            tracing::warn!(error = %e, "advisor refresh failed");
        }
    }
}

/// One sweep: lease expiry, persisted offline transitions, due schedules, and periodic pruning.
fn sweep_once(store: &mut Store, cfg: &Config, prune: bool, now: UnixMs) -> Result<(), SweepError> {
    expire(store, now)?;
    mark_offline(store, cfg, now)?;
    // A schedule problem must not stop lease expiry or pruning.
    if let Err(e) = schedule::fire_due(store, cfg, now) {
        tracing::warn!(error = %e, "schedule sweep failed");
    }
    if prune {
        let days = i64::from(cfg.coordinator.metrics_retention_days);
        let report = store.prune(now, days.saturating_mul(DAY_MS))?;
        tracing::debug!(?report, "store pruned");
    }
    Ok(())
}

/// Re-queues or parks jobs whose lease ran out (agent gone past max runtime plus grace).
fn expire(store: &mut Store, now: UnixMs) -> Result<(), StoreError> {
    let active = store.active_jobs()?;
    let expired = expire_leases(&active, now);
    store.update_jobs(&expired)?;
    for job in &expired {
        let device = active
            .iter()
            .find(|j| j.id == job.id)
            .and_then(|j| j.device.clone());
        let on = device.as_deref().unwrap_or("?");
        let msg = format!(
            "{}: lease expired on {on}, now {}",
            job.spec.name, job.status
        );
        store.append_event(
            EventKind::LeaseExpired,
            device.as_ref(),
            Some(job.id),
            &msg,
            now,
        )?;
    }
    Ok(())
}

/// Persists DeviceOffline once for each online to offline transition.
fn mark_offline(store: &mut Store, cfg: &Config, now: UnixMs) -> Result<(), StoreError> {
    store.persist_offline_transitions(now, offline_after_ms(cfg))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use idlemax_core::protocol::Heartbeat;
    use idlemax_core::types::{DeviceInfo, EventKind};

    use super::{mark_offline, Store};
    use idlemax_core::Config;

    const T0: i64 = 1_800_000_000_000;

    #[test]
    fn restart_does_not_repeat_offline_event() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("coord.db");
        let cfg = Config::default();
        {
            let mut store = Store::open(&db).unwrap();
            store.record_heartbeat(&heartbeat(), T0).unwrap();
            mark_offline(&mut store, &cfg, T0 + 90_001).unwrap();
            assert_eq!(offline_events(&store), 1);
        }
        let mut restarted = Store::open(&db).unwrap();
        mark_offline(&mut restarted, &cfg, T0 + 90_002).unwrap();
        assert_eq!(offline_events(&restarted), 1);
    }

    #[test]
    fn restart_while_online_emits_one_offline_event() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("coord.db");
        let cfg = Config::default();
        {
            let mut store = Store::open(&db).unwrap();
            store.record_heartbeat(&heartbeat(), T0).unwrap();
        }
        {
            let mut restarted = Store::open(&db).unwrap();
            mark_offline(&mut restarted, &cfg, T0 + 90_001).unwrap();
            assert_eq!(offline_events(&restarted), 1);
        }
        let mut restarted = Store::open(&db).unwrap();
        mark_offline(&mut restarted, &cfg, T0 + 90_002).unwrap();
        assert_eq!(offline_events(&restarted), 1);
    }

    fn heartbeat() -> Heartbeat {
        Heartbeat {
            device: DeviceInfo {
                id: "mac".into(),
                ..DeviceInfo::default()
            },
            ..Heartbeat::default()
        }
    }

    fn offline_events(store: &Store) -> usize {
        store
            .events(0)
            .unwrap()
            .iter()
            .filter(|e| e.kind == EventKind::DeviceOffline)
            .count()
    }
}
