//! Domain: the deterministic advisor rules. Always runs; turns cluster state, quota,
//! GitHub items and session metadata into ranked recommendations. Pure.

use crate::advice::ai::AI_PASS_JOB_NAME;
use crate::advice::inputs::{AdviceInput, GhItem, GithubStatus};
use crate::advice::text::{self, DAY_MS, HOUR_MS};
use crate::protocol::DeviceView;
use crate::quota::QuotaView;
use crate::scheduler::{self, SchedContext};
use crate::types::{
    GuardDecision, Job, JobSpec, JobStatus, QuotaWindowKind, Recommendation, RecommendationKind,
    RecommendationSource, RecommendationStatus,
};

const PROBLEM_MAX_AGE_MS: i64 = 7 * DAY_MS;
const OFFLINE_MAX_AGE_MS: i64 = 7 * DAY_MS;
/// A template that succeeded this recently is not suggested again.
const RECENT_RUN_MS: i64 = 20 * HOUR_MS;
const PR_WAIT_MS: i64 = 2 * DAY_MS;
const SESSION_QUIET_MS: i64 = 2 * HOUR_MS;
const SESSION_MAX_AGE_MS: i64 = 3 * DAY_MS;
const ROLLING_RESET_SOON_MS: i64 = 2 * HOUR_MS;
const WEEKLY_RESET_SOON_MS: i64 = DAY_MS;
const UNUSED_MIN_HEADROOM_PCT: f64 = 20.0;

/// All rule findings, most urgent first.
pub fn evaluate(input: &AdviceInput) -> Vec<Recommendation> {
    let active: Vec<Job> = input
        .jobs
        .iter()
        .filter(|j| !j.status.is_terminal())
        .cloned()
        .collect();
    let mut out = problem_jobs(input);
    out.extend(offline_devices(input));
    out.extend(
        input
            .status
            .devices
            .iter()
            .filter_map(|d| idle_device(input, &active, d)),
    );
    out.extend(
        input
            .quota
            .iter()
            .filter_map(|q| unused_window(input, &active, q)),
    );
    out.extend(waiting_prs(input));
    out.extend(unfinished_sessions(input));
    out.extend(github_failure(input));
    out.sort_by(|a, b| {
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then(b.confidence.total_cmp(&a.confidence))
            .then(a.key.cmp(&b.key))
    });
    out
}

fn rank(kind: RecommendationKind) -> u8 {
    match kind {
        RecommendationKind::Warn => 0,
        RecommendationKind::Fix => 1,
        RecommendationKind::Assign => 2,
        RecommendationKind::Do => 3,
        RecommendationKind::FollowUp => 4,
    }
}

fn advice(
    input: &AdviceInput,
    key: String,
    kind: RecommendationKind,
    confidence: f64,
) -> Recommendation {
    Recommendation {
        at_ms: input.now_ms,
        key,
        kind,
        confidence,
        source: RecommendationSource::Rules,
        status: RecommendationStatus::Open,
        ..Default::default()
    }
}

/// Parked or finally failed jobs that no later run of the same job replaced.
fn problem_jobs(input: &AdviceInput) -> Vec<Recommendation> {
    let recent =
        |j: &&Job| input.now_ms - j.finished_ms.unwrap_or(j.created_ms) <= PROBLEM_MAX_AGE_MS;
    input
        .jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Parked | JobStatus::Failed))
        .filter(recent)
        .filter(|j| !superseded(input, j))
        .map(|j| {
            let mut evidence = vec![format!("{} after {} attempt(s)", j.status, j.attempts)];
            evidence.extend(j.last_error.as_deref().map(|e| format!("last error: {}", text::clip(e, 200))));
            evidence.extend(j.exit_code.map(|c| format!("exit code {c}")));
            evidence.extend(j.device.as_deref().map(|d| format!("ran on {d}")));
            evidence.extend(j.output_path.as_deref().map(|p| format!("output: {p}")));
            Recommendation {
                title: text::clip(&format!("Fix job #{} {} ({})", j.id, j.spec.name, j.status), 120),
                why: "It will not run again on its own. Fix the cause, then accept to queue it again."
                    .into(),
                evidence,
                suggested_job: Some(j.spec.clone()),
                ..advice(input, format!("job_problem:{}", j.id), RecommendationKind::Fix, 0.9)
            }
        })
        .collect()
}

fn superseded(input: &AdviceInput, job: &Job) -> bool {
    input.jobs.iter().any(|o| {
        o.spec.name == job.spec.name
            && o.created_ms > job.created_ms
            && (o.status == JobStatus::Succeeded || !o.status.is_terminal())
    })
}

fn offline_devices(input: &AdviceInput) -> Vec<Recommendation> {
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
fn idle_device(input: &AdviceInput, active: &[Job], d: &DeviceView) -> Option<Recommendation> {
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
fn by_priority(a: &JobSpec, b: &JobSpec) -> std::cmp::Ordering {
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

fn ai_template<'a>(input: &'a AdviceInput, account: &str) -> Option<&'a JobSpec> {
    let for_account =
        |t: &&JobSpec| t.ai_account.as_deref() == Some(account) && t.name != AI_PASS_JOB_NAME;
    input
        .templates
        .iter()
        .filter(for_account)
        .max_by(|a, b| by_priority(a, b))
}

fn spend_advice(template: Option<&JobSpec>) -> (RecommendationKind, f64, String) {
    const LOST: &str = "Quota above the reserve is lost at reset";
    match template {
        Some(t) => (
            RecommendationKind::Assign,
            0.7,
            format!("{LOST}; {} would spend part of it.", t.name),
        ),
        None => (
            RecommendationKind::Do,
            0.5,
            format!("{LOST}. Add an AI job template for this account."),
        ),
    }
}

/// A quota window about to reset with plenty above the reserve and no AI job spending it.
fn unused_window(input: &AdviceInput, active: &[Job], q: &QuotaView) -> Option<Recommendation> {
    let (resets_at, used) = (q.resets_at_ms?, q.used_pct?);
    let horizon = match q.window? {
        QuotaWindowKind::Rolling { .. } => ROLLING_RESET_SOON_MS,
        QuotaWindowKind::Weekly => WEEKLY_RESET_SOON_MS,
    };
    let left = resets_at - input.now_ms;
    let spending = active
        .iter()
        .any(|j| j.spec.ai_account.as_deref() == Some(q.account.as_str()));
    if left <= 0 || left > horizon || q.headroom_pct < UNUSED_MIN_HEADROOM_PCT || spending {
        return None;
    }
    let template = ai_template(input, &q.account);
    let window = text::window_label(q.window);
    let key = format!(
        "quota_unused:{}:{}:{resets_at}",
        q.account,
        text::slug(&window)
    );
    let (kind, confidence, why) = spend_advice(template);
    let mut evidence = vec![format!(
        "used {used:.0}%, reserve {:.0}%, spendable {:.0}%",
        q.reserve_pct, q.headroom_pct
    )];
    evidence.extend(q.reasons.iter().take(3).cloned());
    Some(Recommendation {
        title: format!(
            "{} {window} resets in {} with {:.0}% unused",
            q.account,
            text::duration(left),
            100.0 - used
        ),
        why,
        evidence,
        suggested_job: template.cloned(),
        ..advice(input, key, kind, confidence)
    })
}

fn waiting_prs(input: &AdviceInput) -> Vec<Recommendation> {
    let gh = &input.github;
    let review = gh
        .review_requested
        .iter()
        .filter(|i| !i.is_draft)
        .filter_map(|i| stale_pr(input, i, RecommendationKind::Do, "Review", "pr_review"));
    let assigned = gh.assigned_prs.iter().filter_map(|i| {
        stale_pr(
            input,
            i,
            RecommendationKind::FollowUp,
            "Follow up on",
            "pr_assigned",
        )
    });
    review.chain(assigned).collect()
}

fn stale_pr(
    input: &AdviceInput,
    item: &GhItem,
    kind: RecommendationKind,
    verb: &str,
    key_prefix: &str,
) -> Option<Recommendation> {
    let quiet = input.now_ms - item.updated_ms;
    if quiet < PR_WAIT_MS {
        return None;
    }
    Some(Recommendation {
        title: text::clip(&format!("{verb} {}: {}", item.reference(), item.title), 120),
        why: format!("Waiting on you; no activity for {}.", text::duration(quiet)),
        evidence: vec![
            item.url.clone(),
            format!("author {}", item.author),
            format!(
                "opened {} ago",
                text::duration(input.now_ms - item.created_ms)
            ),
        ],
        ..advice(
            input,
            format!("{key_prefix}:{}", item.reference()),
            kind,
            0.8,
        )
    })
}

fn unfinished_sessions(input: &AdviceInput) -> Vec<Recommendation> {
    let now = input.now_ms;
    input
        .sessions
        .iter()
        .filter(|s| !s.signals.is_empty())
        .filter(|s| (SESSION_QUIET_MS..=SESSION_MAX_AGE_MS).contains(&(now - s.last_activity_ms)))
        .map(|s| {
            let name = s.title.as_deref().unwrap_or(&s.id);
            let place = s
                .project
                .as_deref()
                .map(|p| format!(" in {p}"))
                .unwrap_or_default();
            let what: Vec<&str> = s.signals.iter().map(|x| x.describe()).collect();
            let quiet = text::duration(now - s.last_activity_ms);
            let key = format!("session:{}:{}:{}", s.tool, s.id, s.last_activity_ms);
            Recommendation {
                title: text::clip(
                    &format!("Session '{name}'{place} still has {}", what.join(" and ")),
                    120,
                ),
                why: format!("Quiet for {quiet}; it looks unfinished."),
                evidence: vec![format!("{} session {}", s.tool, s.id)],
                ..advice(input, key, RecommendationKind::FollowUp, 0.5)
            }
        })
        .collect()
}

fn github_failure(input: &AdviceInput) -> Option<Recommendation> {
    let GithubStatus::Failed(err) = &input.github.status else {
        return None;
    };
    let key = format!("github_failed:{}", text::day(input.now_ms));
    Some(Recommendation {
        title: "GitHub inputs unavailable".into(),
        why: "The advisor could not read PRs and issues, so PR advice is missing.".into(),
        evidence: vec![text::clip(err, 200)],
        ..advice(input, key, RecommendationKind::Warn, 0.3)
    })
}
