//! Coordinator schedule sweep (adapter): asks the domain which schedules are due and
//! queues one job for each. The guard and scheduler still decide when that job runs.

use idlemax_core::control::admit_job;
use idlemax_core::schedule::{decide, Decision, Schedule};
use idlemax_core::types::{Job, UnixMs};
use idlemax_core::Config;

use super::store::{Store, StoreResult};

/// Queues a job for every due schedule and returns how many it queued. A schedule that
/// fails (a store error) is logged and skipped so the others still fire.
pub fn fire_due(store: &mut Store, cfg: &Config, now: UnixMs) -> StoreResult<usize> {
    let mut fired = 0;
    for schedule in store.list_schedules()? {
        match fire_one(store, cfg, &schedule, now) {
            Ok(true) => fired += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(schedule = schedule.id, error = %e, "schedule sweep failed"),
        }
    }
    Ok(fired)
}

fn fire_one(
    store: &mut Store,
    cfg: &Config,
    schedule: &Schedule,
    now: UnixMs,
) -> StoreResult<bool> {
    let last = last_job(store, schedule)?;
    if decide(schedule, last.as_ref(), now) != Decision::Fire {
        return Ok(false);
    }
    // The config can change after the schedule was added (an AI account removed).
    if let Err(e) = admit_job(&schedule.spec, &cfg.quota) {
        store.defer_schedule(schedule, &e.to_string(), now)?;
        return Ok(false);
    }
    store.fire_schedule(schedule, now)?;
    Ok(true)
}

/// The job the schedule's last fire queued, if it still exists.
pub fn last_job(store: &Store, schedule: &Schedule) -> StoreResult<Option<Job>> {
    match schedule.last_job {
        Some(id) => store.get_job(id),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::schedule::Every;
    use idlemax_core::types::{EventKind, Job, JobSpec, JobStatus};

    use super::*;

    const T0: UnixMs = 1_800_000_000_000;
    const MINUTE: i64 = 60_000;

    fn job_spec(name: &str) -> JobSpec {
        JobSpec {
            name: name.into(),
            command: vec!["true".into()],
            ..JobSpec::default()
        }
    }

    fn queued_for(store: &Store, name: &str) -> Vec<Job> {
        store
            .list_jobs(None, 0)
            .unwrap()
            .into_iter()
            .filter(|j| j.spec.name == name)
            .collect()
    }

    fn finish(store: &mut Store, job: &Job, at_ms: UnixMs) {
        let done = Job {
            status: JobStatus::Succeeded,
            finished_ms: Some(at_ms),
            ..job.clone()
        };
        store.update_jobs(&[done]).unwrap();
    }

    #[test]
    fn a_schedule_fires_once_waits_while_its_job_is_queued_and_fires_again_after_it_ends() {
        let mut store = Store::open_in_memory().unwrap();
        let cfg = Config::default();
        let every: Every = "5m".parse().unwrap();
        store
            .insert_schedule(&job_spec("nightly"), every, T0)
            .unwrap();

        assert_eq!(fire_due(&mut store, &cfg, T0).unwrap(), 1);
        // Hours pass with the job still queued (the machine was never idle): no second job.
        for hours in 1..=3 {
            assert_eq!(
                fire_due(&mut store, &cfg, T0 + hours * 60 * MINUTE).unwrap(),
                0
            );
        }
        let jobs = queued_for(&store, "nightly");
        assert_eq!(jobs.len(), 1);

        // It runs and finishes; the interval counts from the end of the run.
        let ended = T0 + 200 * MINUTE;
        finish(&mut store, &jobs[0], ended);
        assert_eq!(fire_due(&mut store, &cfg, ended + 4 * MINUTE).unwrap(), 0);
        assert_eq!(fire_due(&mut store, &cfg, ended + 5 * MINUTE).unwrap(), 1);
        assert_eq!(fire_due(&mut store, &cfg, ended + 6 * MINUTE).unwrap(), 0);
        assert_eq!(queued_for(&store, "nightly").len(), 2);

        let fired = store
            .events(0)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::ScheduleFired)
            .count();
        assert_eq!(fired, 2);
    }

    #[test]
    fn a_paused_schedule_stays_quiet_and_an_unqueueable_one_says_why() {
        let mut store = Store::open_in_memory().unwrap();
        let cfg = Config::default();
        let every: Every = "5m".parse().unwrap();
        let paused = store
            .insert_schedule(&job_spec("paused"), every, T0)
            .unwrap();
        store.set_schedule_paused(paused.id, true).unwrap();
        let ai = JobSpec {
            cost: idlemax_core::types::CostClass::Ai,
            ai_account: Some("gone".into()),
            ..job_spec("ai")
        };
        store.insert_schedule(&ai, every, T0).unwrap();

        assert_eq!(fire_due(&mut store, &cfg, T0).unwrap(), 0);
        assert!(store.list_jobs(None, 0).unwrap().is_empty());
        let errors: Vec<String> = store
            .events(0)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::Error)
            .map(|e| e.message)
            .collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("\"gone\" is not in [[quota.accounts]]"),
            "{errors:?}"
        );
        // It does not retry (and log) on every sweep, only after the next interval.
        assert_eq!(fire_due(&mut store, &cfg, T0 + MINUTE).unwrap(), 0);
        assert_eq!(store.events(0).unwrap().len(), 1);
    }
}
