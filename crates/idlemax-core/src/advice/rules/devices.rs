//! Rules about devices: one that went offline, and an idle one that could take a job template.
//! Part of the advisor rules (domain layer, pure).

use crate::advice::ai::AI_PASS_JOB_NAME;
use crate::advice::inputs::AdviceInput;
use crate::advice::text::{self, DAY_MS, HOUR_MS};
use crate::protocol::DeviceView;
use crate::scheduler::{self, SchedContext};
use crate::types::{GuardDecision, Job, JobSpec, JobStatus, Recommendation, RecommendationKind};

use super::advice;

const OFFLINE_MAX_AGE_MS: i64 = 7 * DAY_MS;

/// A template that succeeded this recently is not suggested again.
const RECENT_RUN_MS: i64 = 20 * HOUR_MS;

pub(super) fn offline_devices(input: &AdviceInput) -> Vec<Recommendation> {
    let now = input.now_ms;
    input
        .status
        .devices
        .iter()
        .filter(|d| !d.online && now - d.last_seen_ms <= OFFLINE_MAX_AGE_MS)
        .map(|d| {
            let away = text::duration(now - d.last_seen_ms);
            let key = format!("device_offline:{}:{}", d.info.id, d.last_seen_ms);
            Recommendation {
                title: format!("Device {} offline for {away}", d.info.id),
                why: "No heartbeat: it starts no new jobs, and its leases expire so those jobs \
                      re-queue elsewhere."
                    .into(),
                evidence: vec![
                    format!("last heartbeat {away} ago"),
                    format!("{} job(s) were running", d.running.len()),
                ],
                ..advice(input, key, RecommendationKind::Warn, 0.95)
            }
        })
        .collect()
}

fn sched_ctx<'a>(
    input: &'a AdviceInput,
    d: &'a DeviceView,
    guard: &'a GuardDecision,
) -> SchedContext<'a> {
    SchedContext {
        now_ms: input.now_ms,
        device: &d.info,
        guard,
        running: &d.running,
        quota: &input.quota,
        paused: input.status.global_paused || d.paused || d.local_paused,
        paused_accounts: &input.status.paused_accounts,
    }
}

/// An online device whose guard allows work, running nothing, with nothing queued for it.
pub(super) fn idle_device(
    input: &AdviceInput,
    active: &[Job],
    d: &DeviceView,
) -> Option<Recommendation> {
    let guard = d.guard.as_ref()?;
    if !d.online || !d.running.is_empty() || !guard.action.may_start() {
        return None;
    }
    let ctx = sched_ctx(input, d, guard);
    if ctx.paused || !scheduler::pick(&ctx, active).is_empty() {
        return None;
    }
    let id = &d.info.id;
    let key = format!("idle_device:{id}:{}", text::day(input.now_ms));
    let evidence = vec![
        "guard allows new jobs".into(),
        "no queued job fits it".into(),
    ];
    let base = Recommendation {
        evidence,
        ..advice(input, key, RecommendationKind::Assign, 0.6)
    };
    Some(match best_template(input, &ctx, active) {
        Some(t) => Recommendation {
            title: format!("Assign {} to {id} now", t.name),
            why: format!(
                "{id} is idle and nothing queued fits it; {} has not run in the last 20 h.",
                t.name
            ),
            suggested_job: Some(t.clone()),
            ..base
        },
        None => Recommendation {
            kind: RecommendationKind::Do,
            confidence: 0.4,
            title: format!("{id} is idle with nothing to run"),
            why: "No queued job or job template fits it. Add job specs to the jobs folder.".into(),
            ..base
        },
    })
}

fn best_template<'a>(
    input: &'a AdviceInput,
    ctx: &SchedContext<'_>,
    active: &[Job],
) -> Option<&'a JobSpec> {
    input
        .templates
        .iter()
        .filter(|t| {
            t.name != AI_PASS_JOB_NAME
                && !in_flight(active, &t.name)
                && !ran_recently(input, &t.name)
        })
        .filter(|t| {
            let probe = Job {
                spec: (*t).clone(),
                ..Default::default()
            };
            scheduler::blocker(ctx, active, &probe).is_none()
        })
        .max_by(|a, b| by_priority(a, b))
}

/// Highest priority wins; ties go to the alphabetically first name.
pub(super) fn by_priority(a: &JobSpec, b: &JobSpec) -> std::cmp::Ordering {
    a.priority.cmp(&b.priority).then(b.name.cmp(&a.name))
}

fn in_flight(active: &[Job], name: &str) -> bool {
    active.iter().any(|j| j.spec.name == name)
}

fn ran_recently(input: &AdviceInput, name: &str) -> bool {
    input.jobs.iter().any(|j| {
        j.spec.name == name
            && j.status == JobStatus::Succeeded
            && input.now_ms - j.finished_ms.unwrap_or(j.created_ms) < RECENT_RUN_MS
    })
}
