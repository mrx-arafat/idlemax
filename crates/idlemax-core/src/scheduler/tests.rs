//! Tests: scheduler job picking, and the retry / park policy.

use super::*;
use crate::types::JobSpec;

fn job(id: JobId, priority: i32, cost: CostClass, mem_mb: u64) -> Job {
    let ai_account = (cost == CostClass::Ai).then(|| "claude-max".to_string());
    let name = format!("j{id}");
    let spec = JobSpec {
        name,
        priority,
        cost,
        mem_mb,
        ai_account,
        ..Default::default()
    };
    Job {
        id,
        spec,
        created_ms: id,
        ..Default::default()
    }
}

fn active(mut j: Job, status: JobStatus, device: &str) -> Job {
    j.status = status;
    j.device = Some(device.into());
    j
}

fn spendable(ok: bool) -> Vec<QuotaView> {
    let account = "claude-max".to_string();
    vec![QuotaView {
        account,
        headroom_pct: 50.0,
        spendable: ok,
        ..Default::default()
    }]
}

struct Case {
    name: &'static str,
    server: bool,
    guard: GuardAction,
    jobs: Vec<Job>,
    quota: Vec<QuotaView>,
    want: Vec<JobId>,
}

#[test]
fn pick_respects_guard_budget_requirements_profile_and_one_ai_lease_per_account() {
    use CostClass::{Ai, CpuHeavy, CpuLight, Io};
    let run = |slots, mem_mib| GuardAction::Run { slots, mem_mib };
    let mut needs_cargo = job(2, 9, CpuLight, 100);
    needs_cargo.spec.requires = vec!["cargo".into()];
    let mut other_device = job(3, 9, CpuLight, 100);
    other_device.spec.devices = vec!["server".into()];
    let mut large = job(2, 0, Ai, 100);
    large.spec.token_class = Some(TokenClass::Large);
    let cases = vec![
        Case {
            name: "priority first within slots and RAM",
            server: false,
            guard: run(2, 1024),
            jobs: vec![
                job(1, 0, CpuLight, 512),
                job(2, 5, CpuHeavy, 800),
                job(3, 1, Io, 200),
            ],
            quota: vec![],
            want: vec![2, 3],
        },
        Case {
            name: "requires and device list",
            server: false,
            guard: run(2, 1024),
            jobs: vec![job(1, 0, CpuLight, 100), needs_cargo, other_device],
            quota: vec![],
            want: vec![1],
        },
        Case {
            name: "guard hold starts nothing",
            server: false,
            guard: GuardAction::Hold,
            jobs: vec![job(1, 0, CpuLight, 100)],
            quota: vec![],
            want: vec![],
        },
        Case {
            name: "servers take only light and io; an unstarted lease uses the budget",
            server: true,
            guard: run(2, 512),
            jobs: vec![
                job(1, 9, CpuHeavy, 100),
                job(2, 9, Ai, 100),
                job(3, 0, CpuLight, 300),
                job(4, 0, Io, 200),
                active(job(9, 0, Io, 256), JobStatus::Leased, "mac"),
            ],
            quota: spendable(true),
            want: vec![4],
        },
        Case {
            name: "one AI job per account per batch, largest first",
            server: false,
            guard: run(2, 1024),
            jobs: vec![job(1, 0, Ai, 100), large],
            quota: spendable(true),
            want: vec![2],
        },
        Case {
            name: "AI account leased on another device",
            server: false,
            guard: run(2, 1024),
            jobs: vec![
                job(1, 0, Ai, 100),
                active(job(7, 0, Ai, 100), JobStatus::Running, "pi"),
            ],
            quota: spendable(true),
            want: vec![],
        },
        Case {
            name: "AI quota not spendable",
            server: false,
            guard: run(2, 1024),
            jobs: vec![job(1, 0, Ai, 100)],
            quota: spendable(false),
            want: vec![],
        },
    ];
    for c in cases {
        let profile = if c.server {
            DeviceProfile::LinuxServer
        } else {
            DeviceProfile::MacosLaptop
        };
        let device = DeviceInfo {
            id: "mac".into(),
            profile,
            capabilities: vec!["git".into()],
            ai_accounts: vec!["claude-max".into()],
            ..Default::default()
        };
        let guard = GuardDecision {
            action: c.guard,
            ..Default::default()
        };
        let ctx = SchedContext {
            now_ms: 1_000,
            device: &device,
            guard: &guard,
            running: &[],
            quota: &c.quota,
            paused: false,
            paused_accounts: &[],
        };
        assert_eq!(pick(&ctx, &c.jobs), c.want, "{}", c.name);
    }
}

/// What happens to a job after each way an attempt can end: the job's own results are
/// final, and only causes that are not its fault are retried, with backoff, up to
/// `max_attempts`.
#[test]
fn only_causes_that_are_not_the_jobs_fault_are_retried() {
    use CostClass::{Ai, CpuLight};
    use JobStatus::{Failed, Killed, Parked, Queued, Succeeded};
    let running = |attempts: u32, cost: CostClass| {
        let mut j = active(job(1, 0, cost, 100), JobStatus::Running, "mac");
        j.attempts = attempts;
        j.spec.max_attempts = 3;
        j.lease_expires_ms = Some(500);
        j
    };
    // (status, owner_stop, retryable)
    let update = |status: JobStatus, owner_stop: bool, retryable: bool| JobUpdate {
        id: 1,
        device: "mac".into(),
        status,
        at_ms: 900,
        exit_code: (status == Failed).then_some(1),
        owner_stop,
        retryable,
        ..Default::default()
    };
    let final_ = |status| Some((status, None));
    let backoff = |ms| Some((Queued, Some(ms)));
    let cases = [
        (
            "a non-zero exit is final",
            running(1, CpuLight),
            update(Failed, false, false),
            final_(Failed),
        ),
        (
            "so is the last attempt's",
            running(3, CpuLight),
            update(Failed, false, false),
            final_(Failed),
        ),
        (
            "an own-limit kill (memory cap, max runtime) is final",
            running(1, CpuLight),
            update(Killed, false, false),
            final_(Killed),
        ),
        (
            "an owner stop is final",
            running(1, CpuLight),
            update(Killed, true, false),
            final_(Killed),
        ),
        (
            "success is final",
            running(1, CpuLight),
            update(Succeeded, false, false),
            final_(Succeeded),
        ),
        (
            "a guard kill backs off 2 min",
            running(1, CpuLight),
            update(Killed, false, true),
            backoff(120_000),
        ),
        (
            "the second backs off 4 min",
            running(2, CpuLight),
            update(Killed, false, true),
            backoff(240_000),
        ),
        (
            "a start failure is retried too",
            running(1, CpuLight),
            update(Failed, false, true),
            backoff(120_000),
        ),
        (
            "retries stop at max attempts",
            running(3, CpuLight),
            update(Killed, false, true),
            final_(Parked),
        ),
        (
            "an AI job's non-zero exit is final",
            running(1, Ai),
            update(Failed, false, false),
            final_(Failed),
        ),
        (
            "an AI job is never retried",
            running(1, Ai),
            update(Killed, false, true),
            final_(Parked),
        ),
        (
            "another device is ignored",
            running(1, CpuLight),
            JobUpdate {
                device: "pi".into(),
                ..update(Failed, false, true)
            },
            None,
        ),
    ];
    for (name, job, update, want) in cases {
        let got = apply_update(&job, &update, 0).map(|j| (j.status, j.not_before_ms));
        assert_eq!(got, want, "{name}");
    }

    // A lost lease (agent gone, device offline, coordinator restart) is not the job's fault.
    let expired = |attempts, cost| expire_leases(&[running(attempts, cost)], 1_000);
    let outcome = |jobs: Vec<Job>| {
        jobs.iter()
            .map(|j| (j.status, j.not_before_ms))
            .collect::<Vec<_>>()
    };
    assert_eq!(outcome(expired(1, CpuLight)), [(Queued, Some(121_000))]);
    assert_eq!(outcome(expired(3, CpuLight)), [(Parked, None)]);
    assert_eq!(outcome(expired(1, Ai)), [(Parked, None)]);
}

#[test]
fn a_device_that_reports_an_allowlist_only_gets_commands_on_it() {
    let with_command = |id: JobId, program: &str| {
        let mut j = job(id, 0, CostClass::CpuLight, 100);
        j.spec.command = vec![program.into(), "arg".into()];
        j
    };
    let jobs = [with_command(1, "git"), with_command(2, "make")];
    let guard = GuardDecision {
        action: GuardAction::Run {
            slots: 2,
            mem_mib: 1024,
        },
        ..Default::default()
    };
    let pick_on = |allowlist: Option<Vec<String>>| {
        let device = DeviceInfo {
            id: "mac".into(),
            command_allowlist: allowlist,
            ..Default::default()
        };
        let ctx = SchedContext {
            now_ms: 1_000,
            device: &device,
            guard: &guard,
            running: &[],
            quota: &[],
            paused: false,
            paused_accounts: &[],
        };
        pick(&ctx, &jobs)
    };
    assert_eq!(pick_on(Some(vec!["git".into()])), [1]);
    assert_eq!(pick_on(Some(Vec::new())), Vec::<JobId>::new());
    // An agent that predates the field reports nothing, so nothing is filtered.
    assert_eq!(pick_on(None), [1, 2]);
}

#[test]
fn a_result_that_arrives_after_the_lease_expired_counts_if_the_job_has_not_run_again() {
    use CostClass::{Ai, CpuLight};
    use JobStatus::{Failed, Killed, Parked, Queued, Running, Succeeded};
    // The agent was away, the lease expired and the job went back to the queue (device
    // None, attempts unchanged); its replayed result names the attempt it belongs to.
    let expired = |cost: CostClass| {
        let mut j = active(job(1, 0, cost, 100), Running, "mac");
        j.attempts = 1;
        j.lease_expires_ms = Some(500);
        expire_leases(&[j], 1_000).remove(0)
    };
    let update = |status: JobStatus, attempt: u32| JobUpdate {
        id: 1,
        device: "mac".into(),
        status,
        attempt,
        at_ms: 2_000,
        exit_code: Some(0),
        ..Default::default()
    };
    assert_eq!(expired(Ai).status, Parked);
    let mut leased_again = expired(CpuLight);
    leased_again.status = JobStatus::Leased;
    leased_again.device = Some("pi".into());
    leased_again.attempts = 2;
    let mut finished = expired(CpuLight);
    finished.status = Succeeded;
    // (name, stored job, update, wanted (status, device, not_before_ms)).
    let ok = |status, device: Option<&'static str>, not_before| Some((status, device, not_before));
    let cases = [
        (
            "queued job, same attempt",
            expired(CpuLight),
            update(Succeeded, 1),
            ok(Succeeded, Some("mac"), None),
        ),
        (
            "parked AI job, same attempt",
            expired(Ai),
            update(Succeeded, 1),
            ok(Succeeded, Some("mac"), None),
        ),
        (
            "a late failure is final like any other",
            expired(CpuLight),
            update(Failed, 1),
            ok(Failed, Some("mac"), None),
        ),
        (
            "a late guard kill follows the retry policy",
            expired(CpuLight),
            JobUpdate {
                retryable: true,
                ..update(Killed, 1)
            },
            ok(Queued, None, Some(121_000)),
        ),
        (
            "an old agent sends no attempt",
            expired(CpuLight),
            update(Succeeded, 0),
            None,
        ),
        (
            "another attempt",
            expired(CpuLight),
            update(Succeeded, 2),
            None,
        ),
        (
            "leased to another device since",
            leased_again,
            update(Succeeded, 1),
            None,
        ),
        ("already finished", finished, update(Succeeded, 1), None),
        ("not a result", expired(CpuLight), update(Running, 1), None),
    ];
    for (name, job, update, want) in cases {
        let got = apply_update(&job, &update, 1_000).map(|j| {
            (
                j.status,
                j.device
                    .as_deref()
                    .map(|d| if d == "mac" { "mac" } else { "other" }),
                j.not_before_ms,
            )
        });
        assert_eq!(got, want, "{name}");
    }
}
