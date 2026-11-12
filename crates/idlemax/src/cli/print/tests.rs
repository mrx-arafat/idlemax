//! Tests: the one-line cluster summary names every kind of finished-badly job.

use idlemax_core::protocol::{ClusterStatus, JobCounts, LocalStatus};
use idlemax_core::types::{GuardAction, GuardDecision};

use super::{local_status, quota, summary};

#[test]
fn keep_awake_is_a_separate_status_line_only_when_reported() {
    use crate::cli::out::{toon::encode, views};
    use idlemax_core::explain::{DeviceExplanation, ExplainState, Explanation};
    use idlemax_core::protocol::DeviceView;

    let note = "holding (queued work, on AC)";
    let mut status = ClusterStatus {
        devices: vec![DeviceView {
            keep_awake: Some(note.into()),
            info: idlemax_core::types::DeviceInfo {
                id: "mac".into(),
                ..Default::default()
            },
            guard: Some(GuardDecision::default()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut why = Explanation {
        job: 1,
        name: "waiting".into(),
        status: idlemax_core::types::JobStatus::Queued,
        state: ExplainState::Blocked,
        summary: "waiting".into(),
        reasons: vec![],
        at_ms: 0,
        devices: vec![DeviceExplanation {
            memory_limit: None,
            keep_awake: Some(note.into()),
            device: "mac".into(),
            online: true,
            last_seen_ms: 0,
            ready: false,
            ineligible: vec![],
            reasons: vec![],
        }],
    };
    let limit = idlemax_core::types::MemoryLimit {
        configured_mib: 4096,
        effective_mib: 512,
        cgroup_max_bytes: Some(671088640),
    };
    status.devices[0].guard.as_mut().unwrap().memory_limit = Some(limit.clone());
    why.devices[0].memory_limit = Some(limit.clone());
    let message = "RAM budget clamped from 4096 to 512 MiB by effective cgroup memory limit=671088640 bytes (128 MiB reserved for agent)";
    assert!(super::status(&status, 0).contains(message));
    assert!(crate::cli::why::render(&why).contains(message));
    assert!(encode(&views::status(&status, 0, &[]).unwrap()).contains(message));
    assert!(encode(&views::why(&why)).contains(message));
    let local = LocalStatus {
        guard: status.devices[0].guard.clone().unwrap(),
        ..Default::default()
    };
    assert!(local_status(None, &local, 0).contains(message));
    let unknown = idlemax_core::types::MemoryLimit {
        cgroup_max_bytes: None,
        ..limit
    };
    why.devices[0].memory_limit = Some(unknown.clone());
    status.devices[0].guard.as_mut().unwrap().memory_limit = Some(unknown);
    assert!(super::status(&status, 0).contains("reference fallback"));
    assert!(crate::cli::why::render(&why).contains("reference fallback"));
    assert!(serde_json::from_str::<GuardDecision>("{}")
        .unwrap()
        .memory_limit
        .is_none());
    assert!(super::status(&status, 0).contains(&format!("\nmac keep-awake: {note}")));
    assert!(crate::cli::why::render(&why).contains(&format!("\n  keep-awake: {note}")));
    assert!(encode(&views::status(&status, 0, &[]).unwrap()).contains(note));
    assert!(encode(&views::why(&why)).contains(note));
    assert_eq!(
        crate::cli::rows::guard_summary(status.devices[0].guard.as_ref().unwrap()),
        "hold"
    );
    status.devices[0].keep_awake = None;
    why.devices[0].keep_awake = None;
    assert!(!super::status(&status, 0).contains("keep-awake"));
    assert!(!crate::cli::why::render(&why).contains("keep-awake"));
    assert!(!encode(&views::status(&status, 0, &[]).unwrap()).contains("keep_awake"));
    assert!(!encode(&views::why(&why)).contains("keep_awake"));
    let old: LocalStatus = serde_json::from_str("{}").unwrap();
    assert!(old.keep_awake.is_none());
    assert!(serde_json::to_value(old)
        .unwrap()
        .get("keep_awake")
        .is_none());
}

#[test]
fn local_status_prints_the_control_result_before_the_state() {
    let s = LocalStatus {
        device: "mac".into(),
        version: "0.2.0".into(),
        coordinator_reachable: true,
        ..LocalStatus::default()
    };
    assert!(local_status(Some("resumed this device"), &s, 0)
        .starts_with("ok: resumed this device\ndevice mac agent 0.2.0\n"));
    assert!(local_status(None, &s, 0).starts_with("device mac agent 0.2.0\n"));
}

#[test]
fn local_status_lists_every_guard_reason_once_whichever_the_summary_picks() {
    let s = LocalStatus {
        guard: GuardDecision {
            action: GuardAction::Kill,
            reasons: vec![
                "pause: memory pressure Warn".into(),
                "kill: disk free 512 MiB < 1024 MiB".into(),
            ],
            ..GuardDecision::default()
        },
        ..LocalStatus::default()
    };
    let out = local_status(None, &s, 0);
    for reason in &s.guard.reasons {
        assert_eq!(out.matches(&format!("\n  {reason}")).count(), 1, "{out}");
    }
}

fn line(jobs: JobCounts) -> String {
    summary(&ClusterStatus {
        jobs,
        ..ClusterStatus::default()
    })
}

#[test]
fn the_summary_counts_failed_killed_and_parked_jobs_when_there_are_any() {
    let busy = line(JobCounts {
        queued: 2,
        parked: 1,
        failed: 3,
        killed: 4,
        ..JobCounts::default()
    });
    assert!(
        busy.contains("jobs: 0 running, 2 queued, 1 parked, 3 failed, 4 killed |"),
        "{busy}"
    );
    let quiet = line(JobCounts::default());
    assert!(quiet.contains("jobs: 0 running, 0 queued |"), "{quiet}");
}

#[test]
fn status_shows_what_is_used_what_is_idle_and_why_per_device_and_per_account() {
    use crate::cli::out::{toon::encode, views};
    use idlemax_core::capacity::{AccountCapacity, Capacity, CapacityState};
    use idlemax_core::protocol::DeviceView;
    use idlemax_core::types::{DeviceInfo, QuotaWindowKind};

    const NOW: i64 = 1_800_000_000_000;
    let mut status = ClusterStatus {
        devices: vec![DeviceView {
            info: DeviceInfo {
                id: "mac".into(),
                ..Default::default()
            },
            online: true,
            capacity: Some(Capacity {
                state: CapacityState::CannotRun,
                slots_used: 2,
                slots_allowed: Some(4),
                mem_used_mib: 1024,
                mem_allowed_mib: Some(4096),
                why: "2 queued jobs cannot run here: no git".into(),
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    status.accounts = Some(Vec::new());
    let none = encode(&views::status(&status, NOW, &[]).unwrap());
    assert!(
        none.contains("accounts: \"0 AI accounts configured"),
        "{none}"
    );
    status.accounts = Some(vec![AccountCapacity {
        account: "claude".into(),
        headroom_pct: Some(31.0),
        spendable: true,
        window: Some(QuotaWindowKind::Weekly),
        resets_at_ms: Some(NOW + 3 * 3_600_000),
        ..Default::default()
    }]);

    let table = super::status(&status, NOW);
    for want in ["2/4", "1.0G/4.0G", "2 queued jobs cannot run here: no git"] {
        assert!(table.contains(want), "{want} in\n{table}");
    }
    let claude = ["claude", "yes", "31%", "in", "3h00m", "week", "-"];
    let listed = |l: &str| l.split_whitespace().eq(claude);
    assert!(table.lines().any(listed), "{table}");

    let toon = encode(&views::status(&status, NOW, &[]).unwrap());
    assert!(
        toon.contains("devices[1]{id,state,slots,ram,why}:"),
        "{toon}"
    );
    assert!(
        toon.contains(r#"mac,online,2/4,1.0G/4.0G,"2 queued jobs cannot run here: no git""#),
        "{toon}"
    );
    assert!(
        toon.contains("accounts[1]{account,spend,left,resets,window,why}:")
            && toon.contains("claude,yes,31%,in 3h00m,week,"),
        "{toon}"
    );
}

#[test]
fn status_fields_pick_device_columns_and_a_missing_accounts_list_is_not_an_empty_one() {
    use crate::cli::out::{toon::encode, views};

    let status = |accounts: &str| -> ClusterStatus {
        let json = format!(r#"{{"devices":[{{"info":{{"id":"mac"}},"online":true}}]{accounts}}}"#);
        serde_json::from_str(&json).unwrap()
    };
    // `--fields guard,jobs` names device columns; the accounts list must not reject them.
    for fields in [vec!["guard", "jobs"], vec!["all"]] {
        let fields: Vec<String> = fields.into_iter().map(String::from).collect();
        for accounts in ["", r#","accounts":[]"#] {
            let shown = views::status(&status(accounts), 0, &fields);
            assert!(
                shown.is_ok(),
                "{fields:?} with {accounts:?}: {:?}",
                shown.err()
            );
        }
    }
    let wide = ["guard".to_string(), "jobs".to_string()];
    assert!(encode(&views::status(&status(""), 0, &wide).unwrap()).contains("guard"));

    // A coordinator that predates the field sends none: say nothing, not "none configured".
    let old = status("");
    assert!(!encode(&views::status(&old, 0, &[]).unwrap()).contains("accounts"));
    assert!(!super::status(&old, 0).contains("AI accounts"));
    // One that sends an empty list says none are configured.
    let none = status(r#","accounts":[]"#);
    assert!(encode(&views::status(&none, 0, &[]).unwrap()).contains("0 AI accounts configured"));
    assert!(super::status(&none, 0).contains("no AI accounts configured"));
}

#[test]
fn the_budget_table_shows_percent_or_runs_and_why_a_closed_account_runs_no_worker() {
    use idlemax_core::quota::budget::{Budget, BudgetBasis, BudgetUnit};
    use idlemax_core::quota::{Provider, QuotaView};
    const NOW: i64 = 1_800_000_000_000;
    let budget = |account: &str, unit, cap, usable, why: &str| Budget {
        account: account.into(),
        provider: Provider::Claude,
        unit,
        day_start_ms: NOW,
        day_end_ms: NOW + 3_600_000,
        spendable_pct: 7.3,
        spent_pct: 2.0,
        left_pct: 5.3,
        usable,
        rests_until_ms: None,
        owner_active: false,
        cap_runs: cap,
        runs_today: 3,
        costs: Vec::new(),
        basis: if usable {
            BudgetBasis::Learned
        } else {
            BudgetBasis::Paused
        },
        why: why.into(),
    };
    let row = |b: Budget| QuotaView {
        account: b.account.clone(),
        budget: Some(b),
        ..QuotaView::default()
    };
    let views = [
        row(budget(
            "claude-max",
            BudgetUnit::WeeklyPct,
            None,
            true,
            "learned",
        )),
        row(budget(
            "agy-main",
            BudgetUnit::DailyCapRuns,
            Some(5),
            true,
            "daily cap",
        )),
        row(budget(
            "codex",
            BudgetUnit::WeeklyPct,
            None,
            false,
            "paused by you",
        )),
    ];
    let text = quota(&views, &[], NOW);
    assert!(text.contains("claude-max  7.3%"), "{text}");
    assert!(
        text.contains("agy-main    5 runs  3 runs  2 runs  yes"),
        "{text}"
    );
    let codex = text.lines().rfind(|l| l.starts_with("codex ")).unwrap();
    assert!(
        codex.contains(" no ") && codex.ends_with("paused by you"),
        "{codex}"
    );

    let old: QuotaView = serde_json::from_str(r#"{"account":"claude-max"}"#).unwrap();
    assert!(!quota(&[old], &[], NOW).contains("BUDGET"));
}
