//! Tests: the check after a finished turn: the facts a temp repository gives, the verdict
//! they lead to, and a reviewer from another provider asked once on a high-risk diff.

use idlemax_core::types::{JobStatus, QuotaSnapshot, QuotaWindowKind};
use idlemax_core::worker::checks::verdict;
use idlemax_core::worker::report::{ReviewAnswer, WorkerPhase};
use idlemax_core::worker::run::Verdict;
use idlemax_core::worker::{Reviewer, WorkerAction, WorkerCli};

use super::super::host::AgentStatus;
use super::super::Msg;
use super::*;

fn scoped(rig: &Rig) -> Job {
    let mut j = job(rig, WorkerAction::Launch);
    if let Some(w) = j.spec.worker.as_mut() {
        w.scope_paths = vec!["file.txt".into()];
        w.off_limits = vec!["secret/".into()];
        w.review_diff_lines = 300;
    }
    j
}

/// Launches `j`, lets the worker write `path` and commit it, and returns the updates.
fn run_turn(rig: &mut Rig, j: Job, path: &str) -> Vec<JobUpdate> {
    let statuses = [
        AgentStatus::Idle,
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Done,
    ];
    for status in statuses {
        rig.host.push_state(status, None, &[SHELL, AGENT]);
    }
    let mut now = 1_000;
    rig.workers.handle(Msg::Start(Box::new(j)), now, now);
    let mut updates = drive(rig, &mut now, briefed);
    let folder = rig.host.starts.lock().unwrap()[0].folder.clone();
    let file = folder.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "changed\n").unwrap();
    git(&folder, &["add", path]);
    git(&folder, &["commit", "-m", "the worker's change"]);
    updates.extend(drive(rig, &mut now, terminal));
    updates
}

#[test]
fn an_in_scope_commit_is_clean_and_an_off_limits_one_is_rejected() {
    for (path, want) in [
        ("file.txt", Verdict::Clean),
        ("secret/key.txt", Verdict::Rejected),
    ] {
        let mut rig = rig();
        let j = scoped(&rig);
        let worker = j.spec.worker.clone().unwrap();
        let updates = run_turn(&mut rig, j, path);
        let checking = updates.iter().any(|u| {
            u.worker
                .as_ref()
                .is_some_and(|w| w.phase == WorkerPhase::Checking)
        });
        assert!(checking, "no checking phase was reported");
        let end = last(&updates);
        assert_eq!(end.status, JobStatus::Succeeded);
        let facts = end.worker.as_ref().unwrap().facts.clone().unwrap();
        assert_eq!(facts.commits, 1);
        assert_eq!(facts.summary.as_deref(), Some("the worker's change"));
        assert_eq!(
            (facts.baseline_before, facts.baseline_after),
            (Some(0), Some(0))
        );
        assert_eq!(verdict(&facts, &worker).verdict, want, "{path}");
    }
}

#[test]
fn a_high_risk_diff_asks_the_reviewer_once_and_its_concern_needs_the_owner() {
    let mut rig = rig();
    rig.workers
        .set_allowlist(vec!["claude".into(), "codex".into(), "make".into()]);
    let reading = QuotaSnapshot {
        account: "work".into(),
        window: QuotaWindowKind::Weekly,
        used_pct: 12.0,
        resets_at_ms: None,
        source: Default::default(),
        at_ms: 1,
    };
    rig.workers.set_readings(vec![reading]);
    *rig.cmds.review_answer.lock().unwrap() =
        Some(r#"{"verdict":"concern","concern":"it skips the retry path"}"#.into());
    let mut j = scoped(&rig);
    if let Some(w) = j.spec.worker.as_mut() {
        w.review_diff_lines = 0;
        w.reviewer = Some(Reviewer {
            cli: WorkerCli::Codex,
            model: "gpt".into(),
            account: "work".into(),
        });
    }
    let worker = j.spec.worker.clone().unwrap();
    let updates = run_turn(&mut rig, j, "file.txt");
    let facts = last(&updates)
        .worker
        .as_ref()
        .unwrap()
        .facts
        .clone()
        .unwrap();
    let review = facts.review.clone().unwrap();
    assert_eq!(
        (review.account.as_str(), review.model.as_str()),
        ("work", "gpt")
    );
    assert_eq!(review.answer, Some(ReviewAnswer::Concern));
    assert_eq!(review.quota_before.len(), 1);
    assert_eq!(review.quota_after.len(), 1);
    let calls = rig.cmds.calls.lock().unwrap().clone();
    let reviews = calls.iter().filter(|c| c.program == "codex").count();
    assert_eq!(reviews, 1);
    let judged = verdict(&facts, &worker);
    assert_eq!(judged.verdict, Verdict::NeedsYou);
    assert!(judged.concern.unwrap().contains("it skips the retry path"));
}
