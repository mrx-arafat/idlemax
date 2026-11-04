//! Coordinator background sweeps (adapter): lease expiry and offline devices every 15 s,
//! store pruning hourly, advisor refresh every 15 min. Loops log failures and continue.

use std::collections::BTreeSet;
use std::time::Duration;

use idlemax_core::scheduler::expire_leases;
use idlemax_core::types::{DeviceId, EventKind, UnixMs};
use idlemax_core::Config;
use tokio::task::JoinHandle;
use tokio::time::{interval, interval_at, Instant, MissedTickBehavior};

use super::api::{offline_after_ms, AppState};
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

/// Starts the sweep loops; the caller aborts them at shutdown.
pub fn spawn(app: AppState) -> Vec<JoinHandle<()>> {
    let mut tasks = vec![tokio::spawn(maintain(app.clone()))];
    if app.config().advisor.enabled {
        tasks.push(tokio::spawn(advise(app)));
    }
    tasks
}

async fn maintain(app: AppState) {
    let mut tick = interval(SWEEP_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut online = BTreeSet::new();
    let mut last_prune_ms = 0;
    loop {
        tick.tick().await;
        let now = now_ms();
        let prune = now - last_prune_ms >= PRUNE_EVERY_MS;
        let known = std::mem::take(&mut online);
        let result = app
            .with_store(move |store, cfg| sweep_once(store, cfg, known, prune, now))
            .await;
        match result {
            Ok(still_online) => {
                online = still_online;
                if prune {
                    last_prune_ms = now;
                }
            }
            Err(e) => tracing::warn!(error = %e, "coordinator sweep failed"),
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

/// One sweep. Returns the devices online now, so the next sweep can see who went offline.
fn sweep_once(
    store: &mut Store,
    cfg: &Config,
    online: BTreeSet<DeviceId>,
    prune: bool,
    now: UnixMs,
) -> Result<BTreeSet<DeviceId>, SweepError> {
    expire(store, now)?;
    let online = mark_offline(store, cfg, online, now)?;
    if prune {
        let days = i64::from(cfg.coordinator.metrics_retention_days);
        let report = store.prune(now, days.saturating_mul(DAY_MS))?;
        tracing::debug!(?report, "store pruned");
    }
    Ok(online)
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

/// Logs DeviceOffline once when a device online at the last sweep has gone silent.
fn mark_offline(
    store: &mut Store,
    cfg: &Config,
    mut was_online: BTreeSet<DeviceId>,
    now: UnixMs,
) -> Result<BTreeSet<DeviceId>, StoreError> {
    let offline_after = offline_after_ms(cfg);
    let mut online = BTreeSet::new();
    for d in store.devices(now, offline_after)? {
        if d.online {
            online.insert(d.info.id);
        } else if was_online.remove(&d.info.id) {
            let silent_s = now.saturating_sub(d.last_seen_ms) / 1000;
            let msg = format!("{} offline: no heartbeat for {silent_s} s", d.info.id);
            store.append_event(EventKind::DeviceOffline, Some(&d.info.id), None, &msg, now)?;
        }
    }
    Ok(online)
}
