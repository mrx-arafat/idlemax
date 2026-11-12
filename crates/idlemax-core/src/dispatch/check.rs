//! Domain: the focused check of a project on its base branch (D7 source 1): the next step of
//! the base check, and the dispatcher jobs that run its setup and check commands. Pure.
//! This is the only check-job mechanism (A11).

use serde::{Deserialize, Serialize};

use super::fnv64;
use super::inputs::RepoReport;
use super::project::Project;
use crate::types::{
    ActivePolicy, Admission, CostClass, DeviceProfile, Job, JobId, JobSpec, JobStatus, UnixMs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Setup,
    Check,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Setup => "setup",
            Phase::Check => "check",
        }
    }
}

/// The current run of a project's base check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseCheck {
    pub project: i64,
    pub base_commit: String,
    pub phase: Phase,
    pub job: Option<JobId>,
    /// Some(false) is a broken check; None with `finished_ms` is a run that proved nothing.
    pub passed: Option<bool>,
    pub detail: Option<String>,
    pub finished_ms: Option<UnixMs>,
}

/// What to do next for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStep {
    /// The record for this base commit is final.
    Settled,
    /// Nothing to do now, and why.
    Wait(String),
    /// Queue the job of this phase.
    Queue(Phase),
    /// Write the result.
    Done {
        passed: Option<bool>,
        detail: String,
    },
}

/// The next step of a project's base check, from its feed, its record and that record's job.
pub fn next_step(
    p: &Project,
    repo: Option<&RepoReport>,
    rec: Option<&BaseCheck>,
    job: Option<&Job>,
) -> CheckStep {
    if p.settings.check_command.is_empty() {
        return CheckStep::Wait("no check command".into());
    }
    let Some(commit) = repo.and_then(|r| r.base_commit.as_deref()) else {
        return CheckStep::Wait("the base commit is not known yet".into());
    };
    if let Some(why) = checkout_problem(repo, commit) {
        return CheckStep::Wait(why);
    }
    let rec = match rec.filter(|r| r.base_commit == commit) {
        None => return CheckStep::Queue(first_phase(p)),
        Some(r) if r.finished_ms.is_some() => return CheckStep::Settled,
        Some(r) => r,
    };
    let Some(job) = job else {
        return CheckStep::Queue(rec.phase);
    };
    match (job.status, rec.phase) {
        (s, _) if !s.is_terminal() => {
            CheckStep::Wait(format!("the {} job is {s}", rec.phase.as_str()))
        }
        (JobStatus::Succeeded, Phase::Setup) => CheckStep::Queue(Phase::Check),
        (JobStatus::Succeeded, Phase::Check) => CheckStep::Done {
            passed: Some(true),
            detail: "the check passed".into(),
        },
        (JobStatus::Failed, Phase::Check) if job.exit_code.is_some() => CheckStep::Done {
            passed: Some(false),
            detail: ended(job),
        },
        (_, phase) => CheckStep::Done {
            passed: None,
            detail: format!("the {} job {}", phase.as_str(), ended(job)),
        },
    }
}

fn first_phase(p: &Project) -> Phase {
    if p.settings.setup_command.is_empty() {
        Phase::Check
    } else {
        Phase::Setup
    }
}

/// Why the base checkout cannot run the check at `commit`, if it cannot.
fn checkout_problem(repo: Option<&RepoReport>, commit: &str) -> Option<String> {
    match repo.and_then(|r| r.checkout.as_ref()) {
        None => Some("the base checkout is not made yet".into()),
        Some(c) => match (&c.error, c.commit.as_deref()) {
            (Some(e), _) => Some(format!("the base checkout failed: {e}")),
            (None, Some(at)) if at == commit => None,
            _ => Some(format!("the base checkout is not at {commit} yet")),
        },
    }
}

/// "exited 1: <last error>", "was killed" and so on.
fn ended(job: &Job) -> String {
    let mut text = match (job.status, job.exit_code) {
        (JobStatus::Failed, Some(code)) => format!("exited {code}"),
        (status, _) => format!("ended {status}"),
    };
    if let Some(e) = &job.last_error {
        text.push_str(&format!(": {e}"));
    }
    text
}

/// Whether a device of this profile runs base checks: the check job is `cpu_heavy`, which a
/// `production` device never takes, so one queued there would wait for ever (Spec 5 Task 7).
pub fn checks_run_on(profile: DeviceProfile) -> bool {
    crate::scheduler::cost_allowed(profile, CostClass::CpuHeavy)
}

/// The dispatcher job of one phase, run in the base checkout and admitted by the lines (A16).
pub fn check_job(p: &Project, phase: Phase, base_commit: &str, cwd: &str) -> JobSpec {
    let command = match phase {
        Phase::Setup => p.settings.setup_command.clone(),
        Phase::Check => p.settings.check_command.clone(),
    };
    // The command and folder are in the key: after the owner changes a setting, an unfinished
    // job with the old argv is never taken for the new one.
    let mut parts: Vec<&str> = command.iter().map(String::as_str).collect();
    parts.push(cwd);
    let key = format!(
        "dispatch-check:{}:{base_commit}:{}:{}",
        p.id,
        phase.as_str(),
        fnv64(&parts)
    );
    JobSpec {
        name: format!("dispatch-{}:{}", phase.as_str(), p.name),
        key: Some(key),
        command,
        cwd: Some(cwd.to_string()),
        devices: vec![p.device.clone()],
        cost: CostClass::CpuHeavy,
        on_user_active: ActivePolicy::Continue,
        admission: Admission::Lines,
        ..JobSpec::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::inputs::Checkout;
    use crate::dispatch::project::ProjectSettings;

    fn project(setup: bool) -> Project {
        Project {
            id: 7,
            name: "app".into(),
            device: "mac".into(),
            path: "/code/app".into(),
            enabled: true,
            settings: ProjectSettings {
                base_branch: Some("main".into()),
                setup_command: if setup { vec!["make".into()] } else { vec![] },
                check_command: vec!["cargo".into(), "test".into()],
                ..Default::default()
            },
        }
    }

    fn repo() -> RepoReport {
        RepoReport {
            base_commit: Some("c1".into()),
            checkout: Some(Checkout {
                path: "/state/checks/7".into(),
                commit: Some("c1".into()),
                error: None,
            }),
            ..Default::default()
        }
    }

    fn record(phase: Phase) -> BaseCheck {
        BaseCheck {
            project: 7,
            base_commit: "c1".into(),
            phase,
            job: Some(1),
            passed: None,
            detail: None,
            finished_ms: None,
        }
    }

    fn job(status: JobStatus, exit_code: Option<i32>) -> Job {
        Job {
            id: 1,
            status,
            exit_code,
            ..Default::default()
        }
    }

    #[test]
    fn the_base_check_runs_setup_then_check_and_records_how_it_ended() {
        let (p, r) = (project(true), repo());
        assert_eq!(
            next_step(&p, Some(&r), None, None),
            CheckStep::Queue(Phase::Setup)
        );
        let setup = record(Phase::Setup);
        let running = job(JobStatus::Running, None);
        assert!(matches!(
            next_step(&p, Some(&r), Some(&setup), Some(&running)),
            CheckStep::Wait(_)
        ));
        let ok = job(JobStatus::Succeeded, Some(0));
        assert_eq!(
            next_step(&p, Some(&r), Some(&setup), Some(&ok)),
            CheckStep::Queue(Phase::Check)
        );
        let check = record(Phase::Check);
        let failed = job(JobStatus::Failed, Some(1));
        assert!(matches!(
            next_step(&p, Some(&r), Some(&check), Some(&failed)),
            CheckStep::Done {
                passed: Some(false),
                ..
            }
        ));
        let killed = job(JobStatus::Killed, None);
        assert!(matches!(
            next_step(&p, Some(&r), Some(&check), Some(&killed)),
            CheckStep::Done { passed: None, .. }
        ));
        let mut moved = r.clone();
        moved.base_commit = Some("c2".into());
        assert!(matches!(
            next_step(&p, Some(&moved), Some(&check), None),
            CheckStep::Wait(_)
        ));
    }

    #[test]
    fn a_check_job_runs_in_the_checkout_admitted_by_the_lines() {
        let spec = check_job(&project(false), Phase::Check, "c1", "/state/checks/7");
        assert_eq!(spec.admission, Admission::Lines);
        assert_eq!(spec.cost, CostClass::CpuHeavy);
        assert_eq!(spec.cwd.as_deref(), Some("/state/checks/7"));
        assert_eq!(spec.name, "dispatch-check:app");
        assert_eq!(spec.validate(), Ok(()));
    }
}
