//! Tests: the value report's numbers, its verdict line and the guard reason categories.

use super::*;
use crate::config::GuardPolicy;
use crate::guard::Guard;
use crate::types::{
    ControlScope, CostClass, DeviceInfo, DeviceProfile, MemoryPressure, MetricsSnapshot,
    ProbeSource, QuotaWindowKind,
};

const MIN: i64 = 60_000;
const T0: i64 = 1_800_000_000_000;
const GIB: u64 = 1024 * 1024 * 1024;
/// The coordinator's default `offline_after_secs`.
const OFFLINE_AFTER: i64 = 90_000;

fn at(minutes: i64) -> i64 {
    T0 + minutes * MIN
}

fn state(reasons: &[HoldReason]) -> GuardState {
    GuardState {
        held: !reasons.is_empty(),
        reasons: reasons.to_vec(),
    }
}

/// The `mac` device kept `state` from minute `from` to minute `to`.
fn span(from: i64, to: i64, state: GuardState) -> GuardSpan {
    GuardSpan {
        device: "mac".into(),
        start_ms: at(from),
        end_ms: at(to),
        state,
    }
}

/// A job created at minute `created`, not yet started, with nothing that limits where it runs.
fn fact(id: i64, status: JobStatus, created: i64) -> JobFact {
    JobFact {
        id,
        status,
        device: None,
        created_ms: at(created),
        not_before_ms: None,
        started_ms: None,
        finished_ms: None,
        outcome_at_ms: None,
        devices: Vec::new(),
        requires: Vec::new(),
        cost: CostClass::CpuLight,
        ai_account: None,
        program: None,
    }
}

/// A job on `mac` that ran from minute `from` to minute `to`, queued the moment it started.
fn ran(from: i64, to: i64) -> JobFact {
    JobFact {
        device: Some("mac".into()),
        started_ms: Some(at(from)),
        finished_ms: Some(at(to)),
        ..fact(from, JobStatus::Succeeded, from)
    }
}

fn today(minutes: i64) -> Window {
    Window {
        label: "today".into(),
        since_ms: T0,
        until_ms: at(minutes),
    }
}

/// What the report reads besides the guard spans and the job rows.
#[derive(Default)]
struct Extra {
    runs: Vec<JobRun>,
    pauses: Vec<PauseSpan>,
    devices: Vec<DeviceInfo>,
}

fn report(minutes: i64, spans: &[GuardSpan], jobs: &[JobFact]) -> Report {
    report_with(minutes, spans, jobs, &Extra::default())
}

fn report_with(minutes: i64, spans: &[GuardSpan], jobs: &[JobFact], extra: &Extra) -> Report {
    let input = Inputs {
        spans,
        jobs,
        runs: &extra.runs,
        pauses: &extra.pauses,
        devices: &extra.devices,
        quota: &[],
        quota_kept_since_ms: T0,
        accounts_configured: false,
        offline_after_ms: OFFLINE_AFTER,
    };
    build(&today(minutes), &input)
}

fn device(id: &str, profile: DeviceProfile) -> DeviceInfo {
    DeviceInfo {
        id: id.into(),
        profile,
        ..DeviceInfo::default()
    }
}

/// `id` kept `state` from minute `from` to minute `to`.
fn device_span(id: &str, from: i64, to: i64, state: GuardState) -> GuardSpan {
    GuardSpan {
        device: id.into(),
        ..span(from, to, state)
    }
}

#[test]
fn a_day_with_two_jobs_and_six_held_hours_says_where_the_time_went() {
    let memory = state(&[HoldReason::Memory]);
    let spans = [
        span(0, 60, state(&[])),
        span(60, 300, memory),
        span(
            300,
            360,
            state(&[HoldReason::UserActive, HoldReason::Memory]),
        ),
        span(360, 420, state(&[HoldReason::UserActive])),
        span(420, 600, state(&[])),
    ];
    let r = report(600, &spans, &[ran(10, 24), ran(450, 460)]);

    assert_eq!(
        r.verdict,
        "today: 2 jobs ran (24 job-min); guard held 6.0 h, mostly memory pressure (5.0 h); \
         queue was empty 3.6 h of idle time"
    );
    assert_eq!(
        (r.guard.tracked_ms, r.guard.held_ms),
        (600 * MIN, 360 * MIN)
    );
    let by: Vec<(HoldReason, i64)> = r.guard.held_by.iter().map(|h| (h.reason, h.ms)).collect();
    assert_eq!(
        by,
        [
            (HoldReason::Memory, 300 * MIN),
            (HoldReason::UserActive, 120 * MIN)
        ]
    );
    assert_eq!(
        (r.idle.used_ms, r.idle.queue_empty_ms),
        (24 * MIN, 216 * MIN)
    );
    let longest = r.idle.longest.expect("an idle stretch");
    assert_eq!((longest.start_ms, longest.end_ms), (at(24), at(450)));
    assert_eq!(longest.cause, IdleCause::Held(HoldReason::Memory));
    assert_eq!(longest.label, "held by memory pressure");
}

#[test]
fn a_period_without_jobs_names_what_stopped_it() {
    let held = [
        span(0, 480, state(&[HoldReason::Memory])),
        span(480, 600, state(&[])),
    ];
    assert_eq!(
        report(600, &held, &[]).verdict,
        "0 jobs ran today: held 8.0 h by memory pressure"
    );
    let both = [span(
        0,
        120,
        state(&[HoldReason::UserActive, HoldReason::Memory]),
    )];
    assert_eq!(
        report(120, &both, &[]).verdict,
        "0 jobs ran today: held 2.0 h by you using the machine and memory pressure"
    );
    assert_eq!(
        report(120, &both, &[ran(10, 24)]).verdict,
        "today: 1 job ran (14 job-min); guard held 2.0 h, all of it by you using the machine and memory pressure"
    );
    // The stretch is credited to the reason present longest, as the verdict credits it.
    let mixed = [
        span(0, 60, state(&[HoldReason::UserActive, HoldReason::Memory])),
        span(60, 100, state(&[HoldReason::Memory])),
    ];
    let stretch = report(100, &mixed, &[]).idle.longest.expect("a stretch");
    assert_eq!(stretch.cause, IdleCause::Held(HoldReason::Memory));
    let free = [span(0, 600, state(&[]))];
    assert_eq!(
        report(600, &free, &[]).verdict,
        "0 jobs ran today: the queue was empty"
    );
    let queued = fact(2, JobStatus::Queued, 0);
    assert_eq!(
        report(60, &[span(0, 60, state(&[]))], &[queued]).verdict,
        "0 jobs ran today: work was queued but did not start for 58 min, run `idlemax why <id>`"
    );
    assert_eq!(
        report(60, &[], &[]).verdict,
        "0 jobs ran today: no device has reported in this period"
    );
    let lengths = [40_000, 90_000, 3_600_000].map(duration);
    assert_eq!(lengths, ["40 s", "2 min", "1.0 h"]);
}

#[test]
fn silence_longer_than_the_offline_limit_counts_as_asleep_or_offline() {
    let spans = [
        span(0, 60, state(&[])),
        // 30 s after the last heartbeat: the device never went quiet.
        GuardSpan {
            start_ms: at(60) + 30_000,
            ..span(0, 120, state(&[]))
        },
        span(180, 240, state(&[])),
    ];
    let r = report(300, &spans, &[]);
    // 120 to 180 and 240 to 300 had no heartbeat.
    assert_eq!(
        (r.guard.tracked_ms, r.guard.offline_ms),
        (300 * MIN, 120 * MIN)
    );
}

#[test]
fn the_guard_reasons_are_read_from_what_the_real_guard_says() {
    let m = MetricsSnapshot {
        at_ms: 1_000_000,
        cpu_count: 10,
        load_avg_1m: 2.0,
        mem_total_bytes: 16 * GIB,
        mem_available_bytes: 8 * GIB,
        mem_free_pct: 50.0,
        swap_total_bytes: 6 * GIB,
        swap_used_bytes: 4 * GIB,
        disk_free_bytes: 100 * GIB,
        disk_total_bytes: 500 * GIB,
        user_idle_secs: Some(5),
        on_ac_power: Some(false),
        battery_pct: Some(80.0),
        thermal_limited: Some(false),
        memory_pressure: Some(MemoryPressure::Warn),
        ..Default::default()
    };
    let mut guard = Guard::new(GuardPolicy::macos_laptop());
    let decision = guard.evaluate(&m, &[], true, false);
    assert_eq!(
        GuardState::of(&decision).reasons,
        [
            HoldReason::PauseFile,
            HoldReason::UserActive,
            HoldReason::Battery,
            HoldReason::Memory
        ]
    );
    // A host that cannot read the idle time holds new work for a reason of its own; it is not
    // the user being active.
    let unknown = MetricsSnapshot {
        user_idle_secs: None,
        on_ac_power: Some(true),
        memory_pressure: Some(MemoryPressure::Normal),
        ..m.clone()
    };
    let held = Guard::new(GuardPolicy::macos_laptop()).evaluate(&unknown, &[], false, false);
    assert_eq!(GuardState::of(&held).reasons, [HoldReason::IdleUnknown]);
    for (line, reason) in [
        ("pause: swap +300 MiB in 60s > 256 MiB", HoldReason::Memory),
        (
            "pause: load per CPU 1.20 > 1 (12.0 outside the jobs, 10 CPUs)",
            HoldReason::Load,
        ),
        ("hold: disk free 900.0 MiB < 2048 MiB", HoldReason::Disk),
        ("pause: thermal limited", HoldReason::Thermal),
        (
            "hold: recovering, 1/2 clear samples",
            HoldReason::Recovering,
        ),
        ("hold: power source unknown", HoldReason::Battery),
        ("hold: user idle time unknown", HoldReason::IdleUnknown),
        ("pause: local pause active", HoldReason::LocalPause),
    ] {
        assert_eq!(classify(line), Some(reason), "{line}");
    }
    assert_eq!(
        classify("pause: kill-level breach 1/2, pausing first"),
        None
    );
}

#[test]
fn quota_cycles_split_into_expired_and_open_with_what_they_left_unused() {
    let snap = |at_min: i64, used_pct: f64, resets_min: i64| QuotaSnapshot {
        account: "claude".into(),
        window: QuotaWindowKind::Rolling { minutes: 300 },
        used_pct,
        resets_at_ms: Some(at(resets_min)),
        source: ProbeSource::Manual,
        at_ms: at(at_min),
    };
    // The probes round the reset time a second apart; both belong to the first cycle.
    let snaps = [
        snap(60, 20.0, 300),
        QuotaSnapshot {
            resets_at_ms: Some(at(300) + 1000),
            ..snap(240, 55.0, 300)
        },
        snap(360, 10.0, 660),
    ];
    let input = Inputs {
        spans: &[],
        jobs: &[],
        runs: &[],
        pauses: &[],
        devices: &[],
        quota: &snaps,
        quota_kept_since_ms: T0,
        accounts_configured: true,
        offline_after_ms: OFFLINE_AFTER,
    };
    let r = build(&today(480), &input);
    assert!(r.quota.configured);
    let expired: Vec<(f64, f64)> = r
        .quota
        .expired
        .iter()
        .map(|c| (c.peak_used_pct, c.unused_pct))
        .collect();
    assert_eq!(expired, [(55.0, 45.0)]);
    assert_eq!(r.quota.open.len(), 1);
    assert_eq!(r.quota.open[0].peak_used_pct, 10.0);
    assert!(label_ok("last 7 days") && !label_ok("a\nb") && !label_ok(&"x".repeat(40)));

    // Not configured: nothing is reported, whatever the coordinator still holds.
    let unconfigured = Inputs {
        accounts_configured: false,
        ..input
    };
    let q = build(&today(480), &unconfigured).quota;
    assert!(!q.configured && q.expired.is_empty() && q.open.is_empty());

    // Snapshots from before minute 240 are gone, so the cycle that reset at minute 100 is
    // not counted and the report says what it counts from.
    let early = QuotaSnapshot {
        account: "claude".into(),
        window: QuotaWindowKind::Rolling { minutes: 300 },
        used_pct: 90.0,
        resets_at_ms: Some(at(100)),
        source: ProbeSource::Manual,
        at_ms: at(40),
    };
    let all = [early, snaps[0].clone(), snaps[1].clone(), snaps[2].clone()];
    let kept = Inputs {
        quota: &all,
        quota_kept_since_ms: at(240),
        ..input
    };
    let q = build(&today(480), &kept).quota;
    assert_eq!(q.counted_since_ms, at(240));
    let expired: Vec<f64> = q.expired.iter().map(|c| c.unused_pct).collect();
    assert_eq!(expired, [45.0]);
    let everything = Inputs {
        quota: &all,
        ..input
    };
    assert_eq!(build(&today(480), &everything).quota.expired.len(), 2);
}

fn event(job: i64, minute: i64, kind: JobEventKind) -> JobEvent {
    JobEvent {
        job,
        device: Some("mac".into()),
        at_ms: at(minute),
        kind,
    }
}

#[test]
fn a_retried_jobs_first_run_is_run_time_and_not_waiting() {
    // Attempt one ran 10 to 14 and was put back in the queue; attempt two ran 20 to 24. The
    // row keeps only the second: re-leasing clears the first start.
    let events = [
        event(7, 10, JobEventKind::Started),
        event(7, 14, JobEventKind::Ended),
        event(7, 20, JobEventKind::Started),
        event(7, 24, JobEventKind::Ended),
    ];
    let job = JobFact {
        device: Some("mac".into()),
        started_ms: Some(at(20)),
        finished_ms: Some(at(24)),
        ..fact(7, JobStatus::Succeeded, 0)
    };
    let extra = Extra {
        runs: runs_from_events(&events),
        ..Extra::default()
    };
    let r = report_with(60, &[span(0, 60, state(&[]))], &[job], &extra);
    assert_eq!((r.jobs.ran, r.jobs.job_ms), (1, 8 * MIN));
    assert_eq!(r.idle.used_ms, 8 * MIN);
    // Waiting is the 2 to 10 before the first run, past the two minute start grace.
    assert_eq!(r.idle.work_waiting_ms, 8 * MIN);
}

#[test]
fn a_clock_ahead_of_the_coordinator_does_not_turn_a_running_job_into_waiting() {
    // The agent stamped the start ten minutes after the coordinator's "now" of minute 60.
    let job = JobFact {
        device: Some("mac".into()),
        started_ms: Some(at(70)),
        ..fact(3, JobStatus::Running, 50)
    };
    let r = report(60, &[span(0, 60, state(&[]))], &[job]);
    assert_eq!(r.jobs.ran, 1);
    assert_eq!(r.idle.work_waiting_ms, 0);
}

#[test]
fn waiting_counts_only_on_devices_that_could_take_the_job_and_only_once() {
    let infos = vec![
        DeviceInfo {
            ai_accounts: vec!["claude".into()],
            ..device("mac", DeviceProfile::MacosLaptop)
        },
        device("srv", DeviceProfile::LinuxServer),
        device("gone", DeviceProfile::LinuxServer),
    ];
    let extra = Extra {
        devices: infos,
        ..Extra::default()
    };
    let idle = [
        device_span("mac", 0, 60, state(&[])),
        device_span("srv", 0, 60, state(&[])),
    ];
    // Pinned to a server that is offline for the whole period: no idle device could take it.
    let pinned = JobFact {
        devices: vec!["gone".into()],
        ..fact(1, JobStatus::Queued, 0)
    };
    let r = report_with(60, &idle, &[pinned], &extra);
    assert_eq!((r.idle.work_waiting_ms, r.idle.quota_waiting_ms), (0, 0));

    // Free to run on either device: both are idle, but the 58 minutes count once. An AI job
    // that only the laptop may take waits on quota, not on room.
    let free = fact(2, JobStatus::Queued, 0);
    let ai = JobFact {
        cost: CostClass::Ai,
        ai_account: Some("claude".into()),
        ..fact(3, JobStatus::Queued, 0)
    };
    let r = report_with(60, &idle, &[free, ai], &extra);
    assert_eq!(r.idle.work_waiting_ms, 58 * MIN);
    assert_eq!(r.idle.quota_waiting_ms, 58 * MIN);
}

#[test]
fn a_queued_job_no_device_can_take_is_stuck_not_an_empty_queue() {
    let mac = DeviceInfo {
        command_allowlist: Some(vec!["/bin/echo".into()]),
        ..device("mac", DeviceProfile::MacosLaptop)
    };
    let extra = Extra {
        devices: vec![mac],
        ..Extra::default()
    };
    let job = JobFact {
        program: Some("/bin/false".into()),
        ..fact(5, JobStatus::Queued, 0)
    };
    let r = report_with(60, &[span(0, 60, state(&[]))], &[job], &extra);
    assert_eq!(r.idle.stuck_jobs, [5]);
    assert_eq!(
        r.verdict,
        "0 jobs ran today: job 5 is queued and no device can run it, run `idlemax why 5`"
    );
}

#[test]
fn an_offline_device_does_not_speak_for_one_that_was_awake() {
    // The laptop was awake and idle all period; two servers went quiet after the first minute.
    let spans = [
        device_span("mac", 0, 600, state(&[])),
        device_span("srv1", 0, 1, state(&[])),
        device_span("srv2", 0, 1, state(&[])),
    ];
    assert_eq!(
        report(600, &spans, &[]).verdict,
        "0 jobs ran today: the queue was empty"
    );
}

#[test]
fn outcomes_are_counted_on_the_coordinators_clock() {
    // The agent's clock is two minutes behind: it stamped the finish at minute 59, the
    // coordinator logged it at minute 61, inside a period that begins at minute 60.
    let job = JobFact {
        device: Some("mac".into()),
        started_ms: Some(at(50)),
        finished_ms: Some(at(59)),
        outcome_at_ms: Some(at(61)),
        ..fact(4, JobStatus::Succeeded, 40)
    };
    let input = Inputs {
        spans: &[span(0, 120, state(&[]))],
        jobs: &[job],
        runs: &[],
        pauses: &[],
        devices: &[],
        quota: &[],
        quota_kept_since_ms: T0,
        accounts_configured: false,
        offline_after_ms: OFFLINE_AFTER,
    };
    let later = Window {
        label: "later".into(),
        since_ms: at(60),
        until_ms: at(120),
    };
    assert_eq!(build(&later, &input).jobs.succeeded, 1);
}

#[test]
fn a_coordinator_pause_is_a_hold_and_not_work_waiting() {
    let spans = [span(0, 60, state(&[]))];
    let queued = fact(7, JobStatus::Queued, 0);
    let pause = |scope, to: Option<i64>| Extra {
        pauses: vec![PauseSpan {
            scope,
            from_ms: at(0),
            to_ms: to.map(at),
        }],
        ..Extra::default()
    };
    // Everything paused for 40 minutes: the device is held by the pause, and the job only
    // waits for the 20 minutes after it.
    let r = report_with(
        60,
        &spans,
        std::slice::from_ref(&queued),
        &pause(ControlScope::Global, Some(40)),
    );
    assert_eq!(r.guard.held_ms, 40 * MIN);
    assert_eq!(r.guard.held_by[0].reason, HoldReason::Paused);
    assert_eq!(r.idle.work_waiting_ms, 20 * MIN);
    // Only that job paused: the device is free and has nothing it may take.
    let r = report_with(60, &spans, &[queued], &pause(ControlScope::Job(7), None));
    assert_eq!((r.guard.held_ms, r.idle.work_waiting_ms), (0, 0));
    assert_eq!(r.idle.queue_empty_ms, 60 * MIN);
}
