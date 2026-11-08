//! Delivery: spots mutations that would change nothing, so the CLI answers
//! "already X (no-op)" with exit 0 instead of repeating them or failing.
//! Pure: each function reads the state the CLI just fetched.

use idlemax_core::protocol::{ClusterStatus, ControlRequest, LocalStatus};
use idlemax_core::schedule::ScheduleView;
use idlemax_core::types::{
    ControlAction, ControlScope, Job, JobStatus, Recommendation, RecommendationStatus,
};

/// "already paused (no-op)".
pub fn already(state: impl std::fmt::Display) -> String {
    format!("already {state} (no-op)")
}

/// The state a pause, resume or stop-all would only repeat. Scopes whose state the status
/// does not show (one job, or the jobs under a device or account) are never no-ops.
pub fn control(req: &ControlRequest, s: &ClusterStatus) -> Option<&'static str> {
    let device_paused = |id: &str| s.devices.iter().find(|d| d.info.id == id).map(|d| d.paused);
    let active = s.jobs.running + s.jobs.leased + s.jobs.paused;
    match (req.action, &req.scope) {
        (ControlAction::Pause, ControlScope::Global) if s.global_paused => Some("paused"),
        (ControlAction::Pause, ControlScope::Account(a)) if s.paused_accounts.contains(a) => {
            Some("paused")
        }
        (ControlAction::Pause, ControlScope::Device(d)) if device_paused(d) == Some(true) => {
            Some("paused")
        }
        (ControlAction::Resume, ControlScope::Global) if !s.global_paused => Some("running"),
        (ControlAction::Resume, ControlScope::Account(a)) if !s.paused_accounts.contains(a) => {
            Some("running")
        }
        (ControlAction::Resume, ControlScope::Device(d)) if device_paused(d) == Some(false) => {
            Some("running")
        }
        (ControlAction::StopAll, ControlScope::Global) if s.global_paused && active == 0 => {
            Some("stopped")
        }
        _ => None,
    }
}

/// This device's own pause state, for `idlemax local pause|resume|stop-all`.
pub fn local(action: ControlAction, s: &LocalStatus) -> Option<&'static str> {
    match action {
        ControlAction::Pause if s.local_paused => Some("paused"),
        ControlAction::Resume if !s.local_paused => Some("running"),
        ControlAction::StopAll if s.local_paused && s.running.is_empty() => Some("stopped"),
        _ => None,
    }
}

/// Cancelling a job that already ended: the status it ended in.
pub fn cancel(job: &Job) -> Option<JobStatus> {
    job.status.is_terminal().then_some(job.status)
}

pub fn dismiss(rec: &Recommendation) -> bool {
    rec.status == RecommendationStatus::Dismissed
}

/// Pausing a paused schedule, or resuming one that is not paused.
pub fn schedule(view: &ScheduleView, pause: bool) -> bool {
    view.schedule.paused == pause
}

#[cfg(test)]
mod tests {
    use super::*;
    use idlemax_core::protocol::DeviceView;

    fn req(action: ControlAction, scope: ControlScope) -> ControlRequest {
        ControlRequest {
            scope,
            action,
            reason: None,
        }
    }

    fn device(id: &str, paused: bool) -> DeviceView {
        let mut d = DeviceView::default();
        d.info.id = id.into();
        d.paused = paused;
        d
    }

    #[test]
    fn control_is_a_noop_only_when_the_status_shows_the_state_already() {
        use ControlAction::{Pause, Resume, StopAll};
        use ControlScope::{Account, Device, Global, Job};
        let mut s = ClusterStatus {
            devices: vec![device("mac", true), device("box", false)],
            paused_accounts: vec!["claude".into()],
            ..ClusterStatus::default()
        };
        let cases = [
            (Pause, Global, None),
            (Resume, Global, Some("running")),
            (Pause, Account("claude".into()), Some("paused")),
            (Resume, Account("claude".into()), None),
            (Resume, Account("codex".into()), Some("running")),
            (Pause, Device("mac".into()), Some("paused")),
            (Resume, Device("box".into()), Some("running")),
            (Pause, Device("ghost".into()), None),
            (Pause, Job(3), None),
            (Resume, Job(3), None),
            (StopAll, Global, None),
        ];
        for (action, scope, want) in cases {
            assert_eq!(
                control(&req(action, scope.clone()), &s),
                want,
                "{action:?} {scope:?}"
            );
        }
        s.global_paused = true;
        assert_eq!(control(&req(Pause, Global), &s), Some("paused"));
        assert_eq!(control(&req(Resume, Global), &s), None);
        assert_eq!(control(&req(StopAll, Global), &s), Some("stopped"));
        s.jobs.running = 1;
        assert_eq!(
            control(&req(StopAll, Global), &s),
            None,
            "still has a job to kill"
        );
    }

    #[test]
    fn only_finished_jobs_and_dismissed_advice_are_noops() {
        let job = |status| Job {
            status,
            ..Job::default()
        };
        assert_eq!(
            cancel(&job(JobStatus::Succeeded)),
            Some(JobStatus::Succeeded)
        );
        assert_eq!(
            cancel(&job(JobStatus::Cancelled)),
            Some(JobStatus::Cancelled)
        );
        assert_eq!(cancel(&job(JobStatus::Running)), None);
        assert_eq!(cancel(&job(JobStatus::Queued)), None);
        let rec = |status| Recommendation {
            status,
            ..Recommendation::default()
        };
        assert!(dismiss(&rec(RecommendationStatus::Dismissed)));
        assert!(!dismiss(&rec(RecommendationStatus::Open)));
        assert!(!dismiss(&rec(RecommendationStatus::Accepted)));
        assert_eq!(already("paused"), "already paused (no-op)");

        let mut local_status = LocalStatus::default();
        assert_eq!(local(ControlAction::Resume, &local_status), Some("running"));
        assert_eq!(local(ControlAction::Pause, &local_status), None);
        local_status.local_paused = true;
        assert_eq!(local(ControlAction::Pause, &local_status), Some("paused"));
        assert_eq!(
            local(ControlAction::StopAll, &local_status),
            Some("stopped")
        );
    }
}
