//! Coordinator background sweeps (adapter): lease expiry, offline devices and due schedules
//! every 15 s, store pruning hourly, the dispatcher step every minute, advisor refresh every
//! 15 min. Loops log failures and continue.

use std::time::Duration;

use idlemax_core::scheduler::{expire_leases, renew_worker_leases};
use idlemax_core::types::{EventKind, UnixMs};
use idlemax_core::worker::run::after_expiry;
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
/// A gap of more than two sweep periods means the coordinator itself slept: the timer does
/// not count sleep, while leases are compared with the wall clock.
const SLEPT_AFTER_MS: i64 = 2 * SWEEP_EVERY.as_millis() as i64;

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
    // None until the first sweep, so a coordinator start is not taken for a sleep.
    let mut last_sweep_ms: Option<UnixMs> = None;
    loop {
        tick.tick().await;
        let now = now_ms();
        let prune = now - last_prune_ms >= PRUNE_EVERY_MS;
        let slept = last_sweep_ms.is_some_and(|t| now - t > SLEPT_AFTER_MS);
        last_sweep_ms = Some(now);
        match app
            .with_store(move |store, cfg| sweep_once(store, cfg, prune, slept, now))
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
fn sweep_once(
    store: &mut Store,
    cfg: &Config,
    prune: bool,
    slept: bool,
    now: UnixMs,
) -> Result<(), SweepError> {
    expire(store, cfg, now, slept)?;
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
/// A worker job ends instead and its Run goes back in the queue, in the same transaction.
/// After the coordinator `slept`, worker leases are renewed once rather than ended, since the
/// agent on the same machine slept too.
fn expire(store: &mut Store, cfg: &Config, now: UnixMs, slept: bool) -> Result<(), StoreError> {
    let mut active = store.active_jobs()?;
    if slept {
        let renewed = renew_worker_leases(&active, cfg.coordinator.lease_secs, now);
        store.update_jobs(&renewed)?;
        active.retain(|j| !renewed.iter().any(|r| r.id == j.id));
    }
    let expired = expire_leases(&active, now);
    let mut runs = Vec::new();
    for job in expired.iter().filter(|j| j.spec.worker.is_some()) {
        let id = job.spec.worker.as_ref().map_or(0, |w| w.run);
        if let Some(run) = store.get_run(id)? {
            runs.extend(after_expiry(&run, job, now).map(|next| (next, run.state)));
        }
    }
    store.update_jobs_and_runs(&expired, &runs)?;
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

    use super::{expire, mark_offline, Store};
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

    #[test]
    fn a_worker_lease_survives_a_sleep_and_ends_on_the_next_sweep() {
        use idlemax_core::types::{CostClass, JobSpec, JobStatus};
        use idlemax_core::worker::run::{Run, RunLaunch, RunState};
        use idlemax_core::worker::WorkerSpec;

        let mut cfg = Config::default();
        cfg.coordinator.lease_secs = 10;
        let mut store = Store::open_in_memory().unwrap();
        let run = Run {
            device: "mac".into(),
            state: RunState::Running,
            start_ms: T0,
            ..Default::default()
        };
        let mut run = store.insert_run(&run, &RunLaunch::default()).unwrap();
        let spec = JobSpec {
            name: "w".into(),
            command: vec!["idlemax-worker".into(), "claude".into()],
            cost: CostClass::Ai,
            ai_account: Some("max".into()),
            worker: Some(WorkerSpec {
                run: run.id,
                ..Default::default()
            }),
            ..Default::default()
        };
        let job = store
            .insert_leased_job(&spec, "mac", T0 + 10_000, T0)
            .unwrap()
            .into_job();
        run.job = Some(job.id);
        assert!(store.update_run(&run, RunState::Running).unwrap());

        expire(&mut store, &cfg, T0 + 70_000, true).unwrap();
        let kept = store.get_job(job.id).unwrap().unwrap();
        assert_eq!(kept.status, JobStatus::Leased);
        assert_eq!(kept.lease_expires_ms, Some(T0 + 80_000));
        assert_eq!(
            store.get_run(run.id).unwrap().unwrap().state,
            RunState::Running
        );

        expire(&mut store, &cfg, T0 + 85_000, false).unwrap();
        assert_eq!(
            store.get_job(job.id).unwrap().unwrap().status,
            JobStatus::Killed
        );
        assert_eq!(
            store.get_run(run.id).unwrap().unwrap().state,
            RunState::Shed
        );
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
