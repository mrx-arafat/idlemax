//! Tests: how a worker stops: a kill, the runtime cap, a resume after a stop.

use std::sync::Arc;
use std::time::{Duration, Instant};

use idlemax_core::reconcile::KillOrder;
use idlemax_core::types::JobStatus;
use idlemax_core::worker::WorkerAction;

use super::super::host::{AgentStatus, HostError};
use super::super::Msg;
use super::*;

fn kill(rig: &mut Rig, now: i64) {
    let order = KillOrder {
        reason: "stopped by the owner".into(),
        owner_stop: true,
        retryable: false,
    };
    rig.workers.handle(Msg::Kill { id: 7, order }, now, now);
}

#[test]
fn a_kill_keeps_the_worktree_and_ends_only_when_no_process_is_left() {
    for (stubborn, close_fails) in [(false, false), (true, false), (false, true)] {
        let mut rig = rig();
        for status in [AgentStatus::Idle, AgentStatus::Idle, AgentStatus::Working] {
            rig.host.push_state(status, None, &[SHELL, AGENT]);
        }
        if stubborn || close_fails {
            rig.procs.lock().unwrap().stubborn.insert(AGENT);
        }
        if close_fails {
            let err = HostError::Failed {
                call: "herdr pane".into(),
                why: "x".into(),
            };
            rig.host.stop_errors.lock().unwrap().push_back(err);
        }
        let mut now = 1_000;
        rig.workers.handle(
            Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
            now,
            now,
        );
        drive(&mut rig, &mut now, briefed);
        let folder = rig.host.starts.lock().unwrap()[0].folder.clone();
        std::fs::write(folder.join("draft.txt"), "wip").unwrap();
        kill(&mut rig, now);
        now += 1_000;
        let early = rig.workers.tick(now, now);
        if stubborn {
            assert!(early.is_empty(), "ended while a process was alive");
            now += 5_000;
        }
        let mut updates = early;
        if !terminal(&updates) {
            updates = drive(&mut rig, &mut now, terminal);
        }
        assert_eq!(last(&updates).status, JobStatus::Killed);
        assert!(last(&updates).owner_stop);
        assert!(rig.procs.lock().unwrap().live.is_empty());
        assert!(folder.join("draft.txt").exists());
        assert_eq!(rig.host.stopped.lock().unwrap().len(), 1);
    }
}

#[test]
fn a_kill_during_setup_ends_the_setup_and_project_commands_carry_the_fence() {
    let mut rig = rig();
    *rig.cmds.blocking.lock().unwrap() = Some(("make".into(), Arc::clone(&rig.procs)));
    let mut now = 1_000;
    rig.workers.handle(
        Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
        now,
        now,
    );
    let started = Instant::now();
    while !rig.procs.lock().unwrap().tagged.contains(&900) {
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    kill(&mut rig, now);
    let updates = drive(&mut rig, &mut now, terminal);
    assert_eq!(last(&updates).status, JobStatus::Killed);
    assert!(rig.host.starts.lock().unwrap().is_empty());
    let calls = rig.cmds.calls.lock().unwrap().clone();
    let setup = calls.iter().find(|c| c.program == "make").unwrap();
    assert!(setup.throttle);
    assert!(setup.tag.is_some());
    assert!(setup.env.contains(&("IDLEMAX_JOB_ID".into(), "7".into())));
    assert!(setup.env.contains(&("IDLEMAX_TOKEN".into(), String::new())));
}

#[test]
fn the_runtime_cap_counts_awake_time_and_skips_a_paused_worker() {
    let mut rig = rig();
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    let mut spec_job = job(&rig, WorkerAction::Launch);
    spec_job.spec.max_runtime_secs = 60;
    let mut now = 1_000;
    rig.workers.handle(Msg::Start(Box::new(spec_job)), now, now);
    drive(&mut rig, &mut now, briefed);
    rig.workers.handle(Msg::Pause(7), now, now);
    // An idle status while paused does not end the turn, and the clock stands still.
    for _ in 0..5 {
        now += 30_000;
        assert!(rig.workers.tick(now, now).is_empty());
    }
    rig.workers.handle(Msg::Resume(7), now, now);
    rig.host.states.lock().unwrap().clear();
    rig.host
        .push_state(AgentStatus::Working, None, &[SHELL, AGENT]);
    let updates = drive(&mut rig, &mut now, terminal);
    assert_eq!(last(&updates).status, JobStatus::Killed);
    assert!(last(&updates)
        .error
        .as_ref()
        .unwrap()
        .contains("ran longer than 60 s"));
}

#[test]
fn end_text_is_sent_only_for_a_turn_without_a_commit() {
    for commit in [false, true] {
        let mut rig = rig();
        for status in [
            AgentStatus::Idle,
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Working,
            AgentStatus::Done,
        ] {
            rig.host.push_state(status, None, &[SHELL, AGENT]);
        }
        *rig.host.end_text.lock().unwrap() = Some("usage limit reached".into());
        let mut now = 1_000;
        rig.workers.handle(
            Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
            now,
            now,
        );
        drive(&mut rig, &mut now, briefed);
        if commit {
            let folder = rig.host.starts.lock().unwrap()[0].folder.clone();
            std::fs::write(folder.join("file.txt"), "two\n").unwrap();
            git(&folder, &["commit", "-am", "two"]);
        }
        let updates = drive(&mut rig, &mut now, terminal);
        let text = last(&updates).worker.as_ref().unwrap().end_text.clone();
        assert_eq!(text.is_some(), !commit, "commit {commit}");
    }
}

#[test]
fn a_resume_continues_the_saved_session_without_setup() {
    let mut rig = rig();
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    let mut now = 1_000;
    rig.workers.handle(
        Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
        now,
        now,
    );
    drive(&mut rig, &mut now, briefed);
    let folder = rig.host.starts.lock().unwrap()[0].folder.clone();
    kill(&mut rig, now);
    drive(&mut rig, &mut now, terminal);
    rig.procs.lock().unwrap().live.extend([SHELL, AGENT]);
    rig.cmds.calls.lock().unwrap().clear();

    let mut again = job(&rig, WorkerAction::Resume);
    again.id = 8;
    if let Some(w) = again.spec.worker.as_mut() {
        w.session = Some("sess-1".into());
        w.worktree = Some(folder.display().to_string());
        w.branch = Some("idlemax/x".into());
    }
    rig.workers.handle(Msg::Start(Box::new(again)), now, now);
    drive(&mut rig, &mut now, briefed);
    let start = rig.host.starts.lock().unwrap()[1].clone();
    assert!(start.args.windows(2).any(|w| w == ["--resume", "sess-1"]));
    assert_eq!(start.folder, folder);
    assert!(!rig
        .cmds
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.program == "make"));
    assert!(rig.host.sent.lock().unwrap()[1].1.starts_with("Continue"));
}

#[test]
fn a_known_baseline_for_the_base_commit_skips_the_check() {
    for matches in [true, false] {
        let mut rig = rig();
        rig.host
            .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
        let head = std::process::Command::new("git")
            .args(["-C", rig.repo.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap();
        let head = String::from_utf8(head.stdout).unwrap().trim().to_string();
        let mut j = job(&rig, WorkerAction::Launch);
        if let Some(w) = j.spec.worker.as_mut() {
            w.baseline = Some(Baseline {
                base_commit: if matches { head } else { "0".repeat(40) },
                passed: Some(false),
            });
        }
        let mut now = 1_000;
        rig.workers.handle(Msg::Start(Box::new(j)), now, now);
        drive(&mut rig, &mut now, briefed);
        let checks = rig
            .cmds
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.args == ["check"])
            .count();
        assert_eq!(checks, usize::from(!matches), "matches {matches}");
        let folder = rig.host.starts.lock().unwrap()[0].folder.clone();
        let want = if matches { Some(1) } else { Some(0) };
        assert_eq!(
            super::super::worktree::marker_of(&folder).unwrap().baseline,
            want
        );
    }
}

#[test]
fn a_restarted_agent_takes_back_a_matching_worker_and_reports_a_lost_one() {
    for still_there in [true, false] {
        let mut rig = rig();
        rig.host
            .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
        let mut now = 1_000;
        rig.workers.handle(
            Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
            now,
            now,
        );
        drive(&mut rig, &mut now, briefed);
        now += 2_000;
        rig.workers.tick(now, now);
        *rig.host.owned.lock().unwrap() = Some(still_there);

        let mut fresh = Workers::new(
            rig.host.clone(),
            rig.cmds.clone(),
            rig.procs.clone(),
            rig.paths.clone(),
            "mac".into(),
        );
        let updates = fresh.recover(now, now);
        if still_there {
            assert!(updates.is_empty(), "{updates:#?}");
            assert_eq!(fresh.running().len(), 1);
        } else {
            assert_eq!(updates.len(), 1);
            assert_eq!(updates[0].status, JobStatus::Killed);
            assert!(updates[0].retryable);
            assert!(fresh.running().is_empty());
        }
    }
}
