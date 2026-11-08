//! Rules about AI quota: a window about to reset with plenty above the reserve and nothing
//! spending it. Part of the advisor rules (domain layer, pure).

use crate::advice::ai::AI_PASS_JOB_NAME;
use crate::advice::inputs::AdviceInput;
use crate::advice::text::{self, DAY_MS, HOUR_MS};
use crate::quota::QuotaView;
use crate::types::{Job, JobSpec, QuotaWindowKind, Recommendation, RecommendationKind};

use super::advice;
use super::devices::by_priority;

const ROLLING_RESET_SOON_MS: i64 = 2 * HOUR_MS;

const WEEKLY_RESET_SOON_MS: i64 = DAY_MS;

const UNUSED_MIN_HEADROOM_PCT: f64 = 20.0;

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
pub(super) fn unused_window(
    input: &AdviceInput,
    active: &[Job],
    q: &QuotaView,
) -> Option<Recommendation> {
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
