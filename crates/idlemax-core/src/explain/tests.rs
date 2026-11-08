//! Tests: one per blocker family, plus the guard-text coupling and the add-time warning.

use super::*;
use crate::config::GuardPolicy;
use crate::guard::Guard;
use crate::types::{
    CostClass, DeviceProfile, GuardAction, GuardDecision, MemoryPressure, MetricsSnapshot,
};

const NOW: UnixMs = 1_000_000;

fn run(slots: u32, mem_mib: u64) -> GuardDecision {
    let action = GuardAction::Run { slots, mem_mib };
    GuardDecision {
        action,
        resource_action: action,
        ..Default::default()
    }
}

fn device(id: &str) -> DeviceView {
    DeviceView {
        info: DeviceInfo {
            id: id.into(),
            capabilities: vec!["git".into()],
            ai_accounts: vec!["claude-max".into()],
            command_allowlist: Some(vec!["git".into()]),
            ..Default::default()
        },
        online: true,
        last_seen_ms: NOW - 5_000,
        guard: Some(run(2, 1024)),
        ..Default::default()
    }
}

fn queued(id: i64) -> Job {
    let spec = JobSpec {
        name: format!("j{id}"),
        command: vec!["git".into(), "status".into()],
        mem_mb: 100,
        ..Default::default()
    };
    Job {
        id,
        spec,
        created_ms: id,
        ..Default::default()
    }
}

fn ai_job(id: i64) -> Job {
    let mut j = queued(id);
    j.spec.cost = CostClass::Ai;
    j.spec.ai_account = Some("claude-max".into());
    j
}

struct World {
    job: Job,
    others: Vec<Job>,
    devices: Vec<DeviceView>,
    paused: Vec<ControlScope>,
    quota: Vec<QuotaView>,
}

impl World {
    fn new(job: Job) -> Self {
        Self {
            job,
            others: Vec::new(),
            devices: vec![device("mac")],
            paused: Vec::new(),
            quota: Vec::new(),
        }
    }

    fn explain(&self) -> Explanation {
        let mut jobs = self.others.clone();
        jobs.push(self.job.clone());
        explain(&ExplainInput {
            now_ms: NOW,
            job: &self.job,
            jobs: &jobs,
            devices: &self.devices,
            paused: &self.paused,
            quota: &self.quota,
        })
    }
}

fn kinds(reasons: &[Reason]) -> Vec<ReasonKind> {
    reasons.iter().map(|r| r.kind).collect()
}

fn find(reasons: &[Reason], kind: ReasonKind) -> &Reason {
    reasons
        .iter()
        .find(|r| r.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {reasons:?}"))
}

#[test]
fn a_free_job_is_ready_until_other_jobs_take_the_only_slot() {
    let mut w = World::new(queued(1));
    let e = w.explain();
    assert_eq!(e.state, ExplainState::Ready);
    assert!(e.reasons.is_empty() && e.devices[0].ready, "{e:?}");

    w.devices[0].guard = Some(run(1, 1024));
    let mut first = queued(2);
    first.spec.priority = 5;
    w.others.push(first);
    let e = w.explain();
    assert_eq!(e.state, ExplainState::Blocked);
    let order = find(&e.devices[0].reasons, ReasonKind::QueueOrder);
    assert!(order.message.contains("#2"), "{order:?}");

    // A job that started after the device's last heartbeat already holds the slot.
    let mut w = World::new(queued(1));
    w.devices[0].guard = Some(run(1, 1024));
    let mut started = queued(2);
    started.status = JobStatus::Running;
    started.device = Some("mac".into());
    w.others.push(started);
    let e = w.explain();
    assert_eq!(kinds(&e.devices[0].reasons), [ReasonKind::NoSlot]);
    assert!(e.devices[0].reasons[0].message.contains("job #2"), "{e:?}");
}

#[test]
fn pauses_name_their_scope_and_the_command_that_lifts_them() {
    let mut w = World::new(queued(1));
    w.paused = vec![
        ControlScope::Global,
        ControlScope::Job(1),
        ControlScope::Device("mac".into()),
    ];
    let e = w.explain();
    assert_eq!(
        kinds(&e.reasons),
        [ReasonKind::GlobalPause, ReasonKind::JobPause]
    );
    assert_eq!(
        find(&e.reasons, ReasonKind::GlobalPause).fix.as_deref(),
        Some("idlemax resume")
    );
    let job_fix = find(&e.reasons, ReasonKind::JobPause)
        .fix
        .clone()
        .unwrap_or_default();
    assert!(job_fix.contains("idlemax resume --job 1"), "{job_fix}");
    let dev = find(&e.devices[0].reasons, ReasonKind::DevicePause);
    assert_eq!(dev.fix.as_deref(), Some("idlemax resume --device mac"));
}

#[test]
fn no_online_device_is_reported_for_an_empty_cluster_and_for_offline_devices() {
    let mut w = World::new(queued(1));
    w.devices.clear();
    let e = w.explain();
    assert_eq!(kinds(&e.reasons), [ReasonKind::NoDeviceOnline]);
    assert!(
        e.reasons[0].message.contains("no device has connected"),
        "{e:?}"
    );

    w.devices = vec![device("mac")];
    w.devices[0].online = false;
    w.devices[0].last_seen_ms = NOW - 600_000;
    let e = w.explain();
    assert_eq!(kinds(&e.reasons), [ReasonKind::NoDeviceOnline]);
    assert!(
        find(&e.devices[0].reasons, ReasonKind::Offline)
            .message
            .contains("10m"),
        "{e:?}"
    );
}

#[test]
fn device_mismatches_are_listed_per_device_with_the_setting_to_change() {
    let mut job = queued(1);
    job.spec.command = vec!["make".into()];
    job.spec.devices = vec!["mac".into()];
    job.spec.requires = vec!["cargo".into()];
    job.spec.cost = CostClass::CpuHeavy;
    let mut w = World::new(job);
    w.devices[0].info.id = "server".into();
    w.devices[0].info.profile = DeviceProfile::LinuxServer;
    let e = w.explain();
    assert_eq!(
        kinds(&e.devices[0].reasons),
        [
            ReasonKind::DeviceFilter,
            ReasonKind::MissingCapability,
            ReasonKind::CostClass,
            ReasonKind::CommandNotAllowed
        ]
    );
    let fix = find(&e.devices[0].reasons, ReasonKind::CommandNotAllowed)
        .fix
        .clone();
    let fix = fix.unwrap_or_default();
    assert!(
        fix.contains("agent.command_allowlist") && fix.contains("\"make\""),
        "{fix}"
    );
    // The job names a device that never connected: say so once, for the whole job.
    assert!(
        kinds(&e.reasons).contains(&ReasonKind::DeviceFilter),
        "{e:?}"
    );
}

#[test]
fn guard_reasons_carry_the_config_key_and_slots_and_ram_are_counted() {
    let mut guard = Guard::new(GuardPolicy::macos_laptop());
    let on_battery = MetricsSnapshot {
        on_ac_power: Some(false),
        ..calm()
    };
    let decision = guard.evaluate(&on_battery, &[], false, false);
    let mut w = World::new(queued(1));
    w.devices[0].guard = Some(decision);
    let e = w.explain();
    let battery = find(&e.devices[0].reasons, ReasonKind::Guard);
    assert!(battery.message.starts_with("on battery"), "{battery:?}");
    assert_eq!(
        battery.fix.as_deref(),
        Some("plug in, or set guard.require_ac_power = false")
    );

    w.devices[0].guard = Some(run(0, 1024));
    assert_eq!(kinds(&w.explain().devices[0].reasons), [ReasonKind::NoSlot]);
    w.devices[0].guard = Some(run(1, 50));
    assert_eq!(
        kinds(&w.explain().devices[0].reasons),
        [ReasonKind::RamBudget]
    );
}

#[test]
fn every_reason_the_guard_produces_comes_with_a_fix() {
    let storm = MetricsSnapshot {
        mem_free_pct: 5.0,
        mem_available_bytes: 100 << 20,
        swap_used_bytes: 4 << 30,
        load_avg_1m: 9.0,
        disk_free_bytes: 1 << 30,
        user_idle_secs: Some(3),
        on_ac_power: Some(false),
        battery_pct: Some(10.0),
        thermal_limited: Some(true),
        psi_mem_some_avg10: Some(50.0),
        psi_io_full_avg10: Some(50.0),
        ..calm()
    };
    for policy in [GuardPolicy::macos_laptop(), GuardPolicy::linux_server()] {
        let mut guard = Guard::new(policy);
        let first = guard.evaluate(&storm, &[], true, true);
        let calm_after = guard.evaluate(&calm(), &[], false, false);
        for reason in first.reasons.iter().chain(&calm_after.reasons) {
            let r = guard_text::reason(reason);
            assert!(r.fix.is_some(), "no fix for guard reason {reason:?}");
        }
    }
}

#[test]
fn a_guard_reason_names_the_setting_that_holds_the_job() {
    let mut guard = Guard::new(GuardPolicy::macos_laptop());
    let metrics = MetricsSnapshot {
        memory_pressure: Some(MemoryPressure::Warn),
        load_avg_1m: 9.0,
        disk_free_bytes: 1 << 30,
        ..calm()
    };
    let decision = guard.evaluate(&metrics, &[], true, true);
    let fix_for = |what: &str| {
        let raw = decision.reasons.iter().find(|r| r.contains(what));
        let raw = raw.unwrap_or_else(|| panic!("no {what:?} in {:?}", decision.reasons));
        guard_text::reason(raw).fix.unwrap_or_default()
    };

    let pressure = fix_for("memory pressure");
    assert!(
        pressure.contains("guard.memory_pressure_pause"),
        "{pressure}"
    );
    assert!(!pressure.contains("mem_free_pct"), "{pressure}");

    for (what, kill_key, pause_key) in [
        ("load 1m", "guard.load_1m_kill", "guard.load_1m_pause"),
        (
            "disk free",
            "guard.disk_free_mib_kill",
            "guard.disk_free_mib_pause",
        ),
    ] {
        let fix = fix_for(what);
        assert!(fix.contains(kill_key) && fix.contains(pause_key), "{fix}");
    }
}

#[test]
fn a_pause_level_reason_names_only_the_pause_setting() {
    let fix = guard_text::reason("pause: disk free 12000.0 MiB < 15360 MiB").fix;
    let fix = fix.unwrap_or_default();
    assert!(fix.contains("guard.disk_free_mib_pause"), "{fix}");
    assert!(!fix.contains("disk_free_mib_kill"), "{fix}");
}

#[test]
fn ai_jobs_explain_quota_a_busy_account_and_an_account_the_device_may_not_spend() {
    let mut w = World::new(ai_job(1));
    w.paused = vec![ControlScope::Account("claude-max".into())];
    w.devices[0].info.ai_accounts.clear();
    w.quota = vec![QuotaView {
        account: "claude-max".into(),
        headroom_pct: 0.0,
        spendable: false,
        reasons: vec!["5h window not in its final third yet".into()],
        ..Default::default()
    }];
    let mut holder = ai_job(9);
    holder.status = JobStatus::Running;
    holder.device = Some("pi".into());
    w.others.push(holder);
    let e = w.explain();
    assert_eq!(
        kinds(&e.reasons),
        [
            ReasonKind::AccountPause,
            ReasonKind::AiQuota,
            ReasonKind::AiAccountBusy
        ]
    );
    assert!(
        find(&e.reasons, ReasonKind::AiQuota)
            .message
            .contains("final third"),
        "{e:?}"
    );
    assert!(
        find(&e.reasons, ReasonKind::AiAccountBusy)
            .message
            .contains("#9"),
        "{e:?}"
    );
    let on_device = find(&e.devices[0].reasons, ReasonKind::AiAccountNotOnDevice);
    assert!(on_device
        .fix
        .clone()
        .unwrap_or_default()
        .contains("agent.ai_accounts"));
}

#[test]
fn retries_show_the_next_attempt_and_parked_jobs_say_what_to_do() {
    // The stored last_error comes from the scheduler itself, so a reworded one fails here.
    let mut failed = queued(1);
    failed.attempts = 1;
    let mut retry = crate::scheduler::retry_or_park(&failed, NOW - 110_000, "exit 1");
    retry.not_before_ms = Some(NOW + 130_000);
    let e = World::new(retry).explain();
    let backoff = find(&e.reasons, ReasonKind::Backoff);
    assert_eq!(
        backoff.message,
        "waiting out the retry delay: attempt 2 of 3 starts in 2m 10s (last error: exit 1)"
    );
    assert_eq!(backoff.until_ms, Some(NOW + 130_000));

    let mut parked = queued(2);
    parked.status = JobStatus::Parked;
    parked.attempts = 3;
    parked.last_error = Some("exit 1; parked after 3 of 3 attempts".into());
    let e = World::new(parked).explain();
    assert_eq!(e.state, ExplainState::Blocked);
    let r = find(&e.reasons, ReasonKind::Parked);
    assert!(
        r.message.contains("parked after 3 of 3")
            && r.fix
                .clone()
                .unwrap_or_default()
                .contains("idlemax job add"),
        "{r:?}"
    );
}

#[test]
fn running_and_finished_jobs_are_not_blocked() {
    let mut running = queued(1);
    running.status = JobStatus::Running;
    running.device = Some("mac".into());
    running.started_ms = Some(NOW - 60_000);
    let e = World::new(running).explain();
    assert_eq!(e.state, ExplainState::Active);
    assert!(e.summary.contains("running on mac"), "{e:?}");

    let mut done = queued(2);
    done.status = JobStatus::Succeeded;
    let e = World::new(done).explain();
    assert_eq!((e.state, e.reasons.len()), (ExplainState::Finished, 0));
}

#[test]
fn adding_a_job_no_device_can_run_warns_but_unknown_allowlists_do_not() {
    let spec = |devices: &[&str]| JobSpec {
        command: vec!["make".into()],
        devices: devices.iter().map(|d| d.to_string()).collect(),
        ..Default::default()
    };
    let strict = device("mac").info;
    let mut open = device("pi").info;
    open.command_allowlist = Some(vec!["make".into()]);
    let mut old = device("old").info;
    old.command_allowlist = None;

    let warning = allowlist_warning(&spec(&[]), std::slice::from_ref(&strict)).unwrap_or_default();
    assert!(
        warning.contains("\"make\"") && warning.contains("agent.command_allowlist"),
        "{warning}"
    );
    assert_eq!(
        allowlist_warning(&spec(&[]), &[strict.clone(), open.clone()]),
        None
    );
    assert_eq!(allowlist_warning(&spec(&[]), &[strict.clone(), old]), None);
    assert_eq!(allowlist_warning(&spec(&["pi"]), &[strict, open]), None);
    assert_eq!(allowlist_warning(&spec(&[]), &[]), None);
}

fn calm() -> MetricsSnapshot {
    MetricsSnapshot {
        at_ms: NOW,
        cpu_count: 8,
        load_avg_1m: 0.2,
        mem_total_bytes: 16 << 30,
        mem_available_bytes: 8 << 30,
        mem_free_pct: 60.0,
        disk_total_bytes: 500 << 30,
        disk_free_bytes: 300 << 30,
        user_idle_secs: Some(9_999),
        on_ac_power: Some(true),
        battery_pct: Some(90.0),
        thermal_limited: Some(false),
        ..Default::default()
    }
}

#[test]
fn the_headline_is_what_holds_everywhere_else_the_reason_of_the_device_closest_to_running() {
    let headline = |w: &World| {
        let s = summarize(&w.explain());
        let h = s.headline.map(|h| (h.device, h.reason.kind));
        (h, s.more)
    };
    // A reason that holds on every device comes first.
    let mut w = World::new(queued(1));
    w.paused = vec![ControlScope::Job(1)];
    assert_eq!(headline(&w), (Some((None, ReasonKind::JobPause)), 0));

    // The server can never take a heavy job; the laptop only waits for the guard.
    let mut heavy = queued(2);
    heavy.spec.cost = CostClass::CpuHeavy;
    let mut server = device("server");
    server.info.profile = DeviceProfile::LinuxServer;
    let mut laptop = device("mac");
    laptop.guard = Some(GuardDecision {
        action: GuardAction::Pause,
        reasons: vec![
            "pause: on battery".into(),
            "pause: user active 1s ago (< 600s)".into(),
        ],
        ..Default::default()
    });
    let mut w = World::new(heavy);
    w.devices = vec![server, laptop];
    assert_eq!(
        headline(&w),
        (Some((Some("mac".into()), ReasonKind::Guard)), 1)
    );

    // An online device beats an offline one with as many reasons.
    let mut gone = device("gone");
    gone.online = false;
    let mut busy = device("busy");
    busy.guard = Some(GuardDecision {
        action: GuardAction::Hold,
        reasons: vec!["hold: user idle time unknown".into()],
        ..Default::default()
    });
    let mut w = World::new(queued(3));
    w.devices = vec![gone, busy];
    assert_eq!(
        headline(&w),
        (Some((Some("busy".into()), ReasonKind::Guard)), 0)
    );

    // Nothing holds a job that is ready.
    assert_eq!(headline(&World::new(queued(4))), (None, 0));
}
