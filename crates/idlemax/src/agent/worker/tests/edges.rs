//! Tests: the edges of a worker's life: a pause during its launch, an owner stop after a
//! shed, a relaunch over its own worktree, a restart while paused, a host that stops
//! answering, an agent that is not idle when its brief is due, and a Codex server list that
//! cannot be read.

use std::time::{Duration, Instant};

use idlemax_core::reconcile::KillOrder;
use idlemax_core::types::JobStatus;
use idlemax_core::worker::{WorkerAction, WorkerCli};

use super::super::host::AgentStatus;
use super::super::Msg;
use super::*;
use crate::agent::runner::os::Sig;

fn start(rig: &mut Rig, now: i64) {
    let j = job(rig, WorkerAction::Launch);
    rig.workers.handle(Msg::Start(Box::new(j)), now, now);
}

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let started = Instant::now();
    while !ready() {
        assert!(started.elapsed() < Duration::from_secs(5), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn order(retryable: bool, owner_stop: bool) -> KillOrder {
    KillOrder {
        reason: if owner_stop {
            "owner stop"
        } else {
            "line shed"
        }
        .into(),
        owner_stop,
        retryable,
    }
}

#[test]
fn an_owner_stop_after_a_shed_kill_stays_final() {
    let mut rig = rig();
    for status in [AgentStatus::Idle, AgentStatus::Idle, AgentStatus::Working] {
        rig.host.push_state(status, None, &[SHELL, AGENT]);
    }
    rig.procs.lock().unwrap().stubborn.insert(AGENT);
    let mut now = 1_000;
    start(&mut rig, now);
    drive(&mut rig, &mut now, briefed);
    for o in [order(true, false), order(false, true)] {
        rig.workers.handle(Msg::Kill { id: 7, order: o }, now, now);
    }
    now += 6_000;
    let updates = drive(&mut rig, &mut now, terminal);
    assert!(last(&updates).owner_stop);
    assert!(!last(&updates).retryable);
}

#[test]
fn a_pause_during_the_launch_holds_the_agent_start_and_the_brief() {
    let mut rig = rig();
    for status in [AgentStatus::Idle, AgentStatus::Idle, AgentStatus::Working] {
        rig.host.push_state(status, None, &[SHELL, AGENT]);
    }
    let mut now = 1_000;
    start(&mut rig, now);
    rig.workers.handle(Msg::Pause(7), now, now);
    for _ in 0..40 {
        now += 2_000;
        assert!(rig.workers.tick(now, now).is_empty());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(rig.host.starts.lock().unwrap().is_empty());
    rig.workers.handle(Msg::Resume(7), now, now);
    drive(&mut rig, &mut now, briefed);
    assert_eq!(rig.host.sent.lock().unwrap().len(), 1);
}

#[test]
fn a_pause_after_the_agent_started_freezes_it_until_the_brief_can_go() {
    let mut rig = rig();
    rig.host
        .push_state(AgentStatus::Working, None, &[SHELL, AGENT]);
    let mut now = 1_000;
    start(&mut rig, now);
    let host = rig.host.clone();
    wait_for("the agent start", || {
        !host.starts.lock().unwrap().is_empty()
    });
    rig.workers.handle(Msg::Pause(7), now, now);
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    let procs = rig.procs.clone();
    let frozen = move || {
        let signals = procs.lock().unwrap().signals.clone();
        signals.contains(&(vec![SHELL, AGENT], Sig::Stop))
    };
    let started = Instant::now();
    while !frozen() {
        assert!(started.elapsed() < Duration::from_secs(5), "never frozen");
        now += 2_000;
        assert!(rig.workers.tick(now, now).is_empty());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(rig.host.sent.lock().unwrap().is_empty());
    rig.workers.handle(Msg::Resume(7), now, now);
    let signals = rig.procs.lock().unwrap().signals.clone();
    assert!(signals.contains(&(vec![SHELL, AGENT], Sig::Cont)));
    drive(&mut rig, &mut now, briefed);
    assert_eq!(rig.host.sent.lock().unwrap().len(), 1);
}

#[test]
fn a_resumed_turn_without_a_new_commit_sends_its_end_text() {
    let mut rig = rig();
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    let mut now = 1_000;
    start(&mut rig, now);
    drive(&mut rig, &mut now, briefed);
    let folder = rig.host.starts.lock().unwrap()[0].folder.clone();
    std::fs::write(folder.join("file.txt"), "two\n").unwrap();
    git(&folder, &["commit", "-am", "earlier launch"]);
    rig.workers.handle(
        Msg::Kill {
            id: 7,
            order: order(true, false),
        },
        now,
        now,
    );
    drive(&mut rig, &mut now, terminal);

    rig.procs.lock().unwrap().live.extend([SHELL, AGENT]);
    rig.host.states.lock().unwrap().clear();
    for status in [
        AgentStatus::Idle,
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Done,
    ] {
        rig.host.push_state(status, None, &[SHELL, AGENT]);
    }
    *rig.host.end_text.lock().unwrap() = Some("usage limit reached".into());
    let mut again = job(&rig, WorkerAction::Resume);
    again.id = 8;
    if let Some(w) = again.spec.worker.as_mut() {
        w.session = Some("sess-1".into());
        w.worktree = Some(folder.display().to_string());
    }
    rig.workers.handle(Msg::Start(Box::new(again)), now, now);
    let updates = drive(&mut rig, &mut now, terminal);
    let text = last(&updates).worker.as_ref().unwrap().end_text.clone();
    assert_eq!(text.as_deref(), Some("usage limit reached"));
}

#[test]
fn a_relaunch_over_its_own_worktree_runs_no_setup_and_no_check() {
    let mut rig = rig();
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    let mut now = 1_000;
    start(&mut rig, now);
    drive(&mut rig, &mut now, briefed);
    let folder = rig.host.starts.lock().unwrap()[0].folder.clone();
    std::fs::write(folder.join("file.txt"), "half done\n").unwrap();
    rig.workers.handle(
        Msg::Kill {
            id: 7,
            order: order(true, false),
        },
        now,
        now,
    );
    drive(&mut rig, &mut now, terminal);
    rig.procs.lock().unwrap().live.extend([SHELL, AGENT]);
    rig.cmds.calls.lock().unwrap().clear();

    let mut again = job(&rig, WorkerAction::Launch);
    again.id = 8;
    if let Some(w) = again.spec.worker.as_mut() {
        w.worktree = Some(folder.display().to_string());
    }
    rig.workers.handle(Msg::Start(Box::new(again)), now, now);
    drive(&mut rig, &mut now, briefed);
    assert_eq!(rig.host.starts.lock().unwrap()[1].folder, folder);
    assert!(!rig
        .cmds
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.program == "make"));
    assert_eq!(
        super::super::worktree::marker_of(&folder).unwrap().baseline,
        Some(0)
    );
}

#[test]
fn a_worker_paused_before_a_restart_comes_back_paused() {
    let mut rig = rig();
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    let mut now = 1_000;
    start(&mut rig, now);
    drive(&mut rig, &mut now, briefed);
    rig.workers.handle(Msg::Pause(7), now, now);
    now += 2_000;
    rig.workers.tick(now, now);

    let mut fresh = Workers::new(
        rig.host.clone(),
        rig.cmds.clone(),
        rig.procs.clone(),
        rig.paths.clone(),
        "mac".into(),
    );
    assert!(fresh.recover(now, now).is_empty());
    assert!(fresh.jobs()[0].1);
    assert_eq!(fresh.running()[0].status, JobStatus::Paused);
}

#[test]
fn a_host_that_stops_answering_fails_the_worker_after_a_minute() {
    let mut rig = rig();
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    let mut now = 1_000;
    start(&mut rig, now);
    drive(&mut rig, &mut now, briefed);
    *rig.host.state_error.lock().unwrap() = true;
    now += 2_000;
    assert!(rig.workers.tick(now, now).is_empty());
    now += 59_000;
    assert!(rig.workers.tick(now, now).is_empty());
    let updates = drive(&mut rig, &mut now, terminal);
    assert_eq!(last(&updates).status, JobStatus::Failed);
    assert!(last(&updates)
        .error
        .as_ref()
        .unwrap()
        .contains("did not answer"));
}

#[test]
fn a_brief_is_never_sent_to_an_agent_that_is_not_idle() {
    let mut rig = rig();
    rig.workers.idle_wait = Duration::from_millis(200);
    rig.host
        .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    rig.host
        .push_state(AgentStatus::Blocked, None, &[SHELL, AGENT]);
    let mut now = 1_000;
    start(&mut rig, now);
    let updates = drive(&mut rig, &mut now, terminal);
    assert_eq!(last(&updates).status, JobStatus::Failed);
    assert!(last(&updates)
        .error
        .as_ref()
        .unwrap()
        .contains("did not reach idle"));
    assert!(rig.host.sent.lock().unwrap().is_empty());
}

#[test]
fn a_codex_server_list_that_cannot_be_read_fails_the_launch() {
    let mut rig = rig();
    rig.workers
        .set_allowlist(vec!["codex".into(), "make".into()]);
    rig.cmds.script.lock().unwrap().insert("codex".into(), 1);
    let mut j = job(&rig, WorkerAction::Launch);
    if let Some(w) = j.spec.worker.as_mut() {
        w.cli = WorkerCli::Codex;
    }
    let mut now = 1_000;
    rig.workers.handle(Msg::Start(Box::new(j)), now, now);
    let updates = drive(&mut rig, &mut now, terminal);
    assert_eq!(last(&updates).status, JobStatus::Failed);
    assert!(last(&updates)
        .error
        .as_ref()
        .unwrap()
        .contains("codex mcp list"));
    assert!(rig.host.starts.lock().unwrap().is_empty());
}
