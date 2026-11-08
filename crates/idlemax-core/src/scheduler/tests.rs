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

#[test]
fn failures_back_off_then_park_ai_never_retries_and_owner_stops_are_final() {
    let running = |attempts: u32, cost: CostClass| {
        let mut j = active(job(1, 0, cost, 100), JobStatus::Running, "mac");
        j.attempts = attempts;
        j.spec.max_attempts = 3;
        j.lease_expires_ms = Some(500);
        j
    };
    let update = |status: JobStatus, owner_stop: bool| JobUpdate {
        id: 1,
        device: "mac".into(),
        status,
        at_ms: 900,
        owner_stop,
        ..Default::default()
    };
    use CostClass::{Ai, CpuLight};
    use JobStatus::{Failed, Killed, Parked, Queued};
    let cases = [
        (
            "first failure backs off 2 min",
            running(1, CpuLight),
            update(Failed, false),
            Some((Queued, Some(120_000))),
        ),
        (
            "second failure backs off 4 min",
            running(2, CpuLight),
            update(Failed, false),
            Some((Queued, Some(240_000))),
        ),
        (
            "max attempts parks",
            running(3, CpuLight),
            update(Failed, false),
            Some((Parked, None)),
        ),
        (
            "AI job parks at once",
            running(1, Ai),
            update(Failed, false),
            Some((Parked, None)),
        ),
        (
            "guard kill retries",
            running(1, CpuLight),
            update(Killed, false),
            Some((Queued, Some(120_000))),
        ),
        (
            "owner stop is final",
            running(1, CpuLight),
            update(Killed, true),
            Some((Killed, None)),
        ),
        (
            "other device is ignored",
            running(1, CpuLight),
            JobUpdate {
                device: "pi".into(),
                ..update(Failed, false)
            },
            None,
        ),
    ];
    for (name, job, update, want) in cases {
        let got = apply_update(&job, &update, 0).map(|j| (j.status, j.not_before_ms));
        assert_eq!(got, want, "{name}");
    }
    let expired = expire_leases(&[running(1, CpuLight)], 1_000);
    assert_eq!(
        expired
            .iter()
            .map(|j| (j.status, j.not_before_ms))
            .collect::<Vec<_>>(),
        [(Queued, Some(121_000))]
    );
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
