//! Tests: quota spend rules.

use super::*;
use crate::quota::{rolling_5h, Provider};
use crate::types::ProbeSource;

const NOW: i64 = 1_800_000_000_000;
const MIN: i64 = MINUTE_MS;
const HOUR: i64 = 60 * MIN;
const WEEKLY: QuotaWindowKind = QuotaWindowKind::Weekly;

#[derive(Default)]
struct Fx {
    snaps: Vec<QuotaSnapshot>,
    interactive: Vec<(String, i64)>,
    leased: Vec<String>,
    hits: Vec<(String, i64)>,
    starts: Vec<(String, i64)>,
}

impl Fx {
    fn ctx(&self) -> SpendContext<'_> {
        SpendContext {
            now_ms: NOW,
            snapshots: &self.snaps,
            interactive_use: &self.interactive,
            leased_accounts: &self.leased,
            limit_hits: &self.hits,
            ai_job_starts: &self.starts,
        }
    }
}

fn snap(
    account: &str,
    window: QuotaWindowKind,
    used: f64,
    resets_in: i64,
    age: i64,
) -> QuotaSnapshot {
    QuotaSnapshot {
        account: account.into(),
        window,
        used_pct: used,
        resets_at_ms: Some(NOW + resets_in),
        source: ProbeSource::Manual,
        at_ms: NOW - age,
    }
}

/// Claude 5h window in its final third (resets in 60 of 300 min), both windows under reserve.
fn claude_base() -> Fx {
    Fx {
        snaps: vec![
            snap("claude", rolling_5h(), 30.0, 60 * MIN, 10 * MIN),
            snap("claude", WEEKLY, 50.0, 72 * HOUR, 10 * MIN),
        ],
        ..Fx::default()
    }
}

fn cfg() -> QuotaConfig {
    let account = |name: &str, provider| AccountConfig {
        name: name.into(),
        provider,
        ..AccountConfig::default()
    };
    QuotaConfig {
        accounts: vec![
            account("claude", Provider::Claude),
            account("codex", Provider::Codex),
        ],
        ..QuotaConfig::default()
    }
}

type Case = (&'static str, fn(&mut Fx), bool);

#[test]
fn spend_rules_reserve_final_third_interactive_hold_and_one_lease_per_account() {
    let cases: [Case; 11] = [
        ("base: final third, under reserve", |_| {}, true),
        (
            "5h not in final third",
            |f| f.snaps[0].resets_at_ms = Some(NOW + 150 * MIN),
            false,
        ),
        (
            "5h used reaches 100 - reserve",
            |f| f.snaps[0].used_pct = 60.0,
            false,
        ),
        (
            "weekly used reaches 100 - reserve",
            |f| f.snaps[1].used_pct = 70.0,
            false,
        ),
        (
            "weekly final day: 15% reserve",
            |f| {
                f.snaps.remove(0);
                f.snaps[0].used_pct = 80.0;
                f.snaps[0].resets_at_ms = Some(NOW + 20 * HOUR);
            },
            true,
        ),
        (
            "interactive 10 min ago",
            |f| f.interactive.push(("claude".into(), NOW - 10 * MIN)),
            false,
        ),
        (
            "interactive 40 min ago",
            |f| f.interactive.push(("claude".into(), NOW - 40 * MIN)),
            true,
        ),
        (
            "lease held elsewhere",
            |f| f.leased.push("claude".into()),
            false,
        ),
        (
            "limit hit this window",
            |f| f.hits.push(("claude".into(), NOW - 30 * MIN)),
            false,
        ),
        (
            "AI job 2 h ago",
            |f| f.starts.push(("claude".into(), NOW - 2 * HOUR)),
            false,
        ),
        ("no data", |f| f.snaps.clear(), false),
    ];
    for (name, mutate, want) in cases {
        let mut fx = claude_base();
        mutate(&mut fx);
        assert_eq!(spendable(&cfg(), "claude", &fx.ctx()), want, "{name}");
    }
}

#[test]
fn a_limit_hit_marks_every_row_until_its_window_resets() {
    let mut fx = claude_base();
    fx.hits = vec![("claude".into(), NOW - 10 * MIN)];
    let rows = views(&cfg(), &fx.ctx());
    let claude: Vec<&QuotaView> = rows.iter().filter(|v| v.account == "claude").collect();
    assert!(
        claude.len() == 2 && claude.iter().all(|v| v.limited.is_some()),
        "{claude:?}"
    );
    assert!(rows
        .iter()
        .filter(|v| v.account == "codex")
        .all(|v| v.limited.is_none()));
    // The 5-hour window that saw the hit has reset: the next one starts after it.
    fx.snaps[0] = snap("claude", rolling_5h(), 5.0, 295 * MIN, MIN);
    let rows = views(&cfg(), &fx.ctx());
    assert!(rows.iter().all(|v| v.limited.is_none()), "{rows:?}");
    let old: QuotaView = serde_json::from_str(r#"{"account":"claude"}"#).unwrap();
    assert_eq!(old.limited, None);
}

#[test]
fn a_worker_end_text_is_a_limit_by_either_phrase_list_or_an_http_429() {
    for yes in [
        "Error 429: Too Many Requests",
        "You've hit your limit",
        "weekly limit reached",
        "working on it\nusage limit reached",
        "request failed with status 429",
    ] {
        assert!(is_limit_message(yes), "{yes}");
    }
    for no in ["HTTP 200 OK", "build 4290 passed"] {
        assert!(!is_limit_message(no), "{no}");
    }
    use crate::job_run::rate_limited;
    use crate::types::CostClass;
    assert!(!rate_limited(CostClass::Ai, "session limit"));
    assert!(rate_limited(CostClass::Ai, "usage limit"));

    // The live false hit: a `claude -p` result line with "is_error":false and 429 thinking
    // tokens; and a pick reason that only mentions a rate limiter.
    let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens_details":{"thinking_tokens":429}},"structured_output":{"picks":[{"reason":"fix the rate limit middleware"}]}}"#;
    assert!(!rate_limited(CostClass::Ai, ok));
    let hit = r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached|1794520000"}"#;
    assert!(rate_limited(CostClass::Ai, hit));
    let api = r#"{"type":"result","is_error":true,"result":"API Error: 429 {\"type\":\"rate_limit_error\"}"}"#;
    assert!(rate_limited(CostClass::Ai, api));
}
