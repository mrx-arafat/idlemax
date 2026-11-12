//! Tests: the Herdr host builds its calls from fixture JSON in the observed shapes, through
//! `FakeCommands`; no test starts `herdr`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use idlemax_core::worker::WorkerCli;

use super::*;
use crate::agent::worker::cmd::{CmdError, Commands, FakeCommands, Output};

fn ok(stdout: &str) -> Result<Output, CmdError> {
    Ok(Output {
        code: Some(0),
        stdout: stdout.into(),
        stderr: String::new(),
    })
}

fn launch() -> Launch {
    Launch {
        name: "im-app-fix-1".into(),
        tab: "im: App".into(),
        folder: "/w/app/fix".into(),
        env: vec![("IDLEMAX_JOB_ID".into(), "7".into())],
        cli: WorkerCli::Claude,
        args: vec!["--model".into(), "sonnet".into()],
        job: 7,
        attempt: 1,
        run: 3,
        worktrees_dir: "/w".into(),
    }
}

const WORKSPACES: &str = r#"{"id":"1","result":{"workspaces":[
    {"workspace_id":"w0","label":"owner"},{"workspace_id":"w1","label":"idlemax workers"}]}}"#;
const TABS: &str = r#"{"id":"2","result":{"tabs":[{"tab_id":"t1","label":"im: App"}]}}"#;
const PANES_BEFORE: &str = r#"{"result":{"panes":[
    {"pane_id":"p1","label":"im-app-old-1","workspace_id":"w1","tab_id":"t1"}]}}"#;
const PANES_AFTER: &str = r#"{"result":{"panes":[
    {"pane_id":"p1","label":"im-app-old-1","workspace_id":"w1","tab_id":"t1"},
    {"pane_id":"p2","label":null,"workspace_id":"w1","tab_id":"t1"}]}}"#;
const INFO: &str = r#"{"result":{"process_info":{"shell_pid":4242,
    "foreground_processes":[{"pid":4300,"name":"claude"}]}}}"#;

/// A fake Herdr with the worker workspace and the project's tab already there.
fn existing_tab() -> Arc<FakeCommands> {
    let fake = Arc::new(FakeCommands::new());
    let pane_lists = Arc::new(AtomicUsize::new(0));
    fake.set_fallback(move |cmd| {
        let line = cmd.args.join(" ");
        match line.as_str() {
            "workspace list" => ok(WORKSPACES),
            "tab list --workspace w1" => ok(TABS),
            l if l.starts_with("pane list") => {
                let n = pane_lists.fetch_add(1, Ordering::SeqCst);
                ok(if n == 0 { PANES_BEFORE } else { PANES_AFTER })
            }
            l if l.starts_with("pane process-info") => ok(INFO),
            _ => ok(r#"{"result":{}}"#),
        }
    });
    fake
}

fn lines(fake: &FakeCommands) -> Vec<String> {
    fake.calls()
        .iter()
        .map(|c| format!("{} {}", c.program, c.args.join(" ")))
        .collect()
}

#[test]
fn a_start_splits_the_projects_tab_names_the_pane_and_starts_the_agent() {
    let fake = existing_tab();
    let host = HerdrHost::with_background(fake.clone(), Some("taskpolicy".into()));
    let handle = host.start(&launch()).unwrap();
    assert_eq!(
        handle,
        Handle {
            pane: "p2".into(),
            workspace: "w1".into(),
            label: "im-app-fix-1".into(),
        }
    );
    let calls = lines(&fake);
    for want in [
        "herdr pane split --pane p1 --direction down --cwd /w/app/fix --env IDLEMAX_JOB_ID=7 --no-focus",
        "herdr pane rename p2 im-app-fix-1",
        "taskpolicy -b -p 4242",
        "herdr agent start im-app-fix-1 --kind claude --pane p2 -- --model sonnet",
    ] {
        assert!(calls.iter().any(|c| c == want), "{want} not in {calls:#?}");
    }
    let focus = calls
        .iter()
        .flat_map(|c| c.split(' '))
        .any(|w| w == "--focus" || w == "focus");
    assert!(!focus, "{calls:#?}");
    assert!(fake.calls().iter().all(|c| c.limit == HERDR_LIMIT));
}

#[test]
fn a_first_start_makes_the_workspace_and_names_its_tab() {
    let fake = Arc::new(FakeCommands::new());
    let made = Arc::new(AtomicUsize::new(0));
    let seen = made.clone();
    fake.set_fallback(move |cmd| {
        let line = cmd.args.join(" ");
        if line.starts_with("workspace create") {
            seen.store(1, Ordering::SeqCst);
        }
        let exists = seen.load(Ordering::SeqCst) == 1;
        match line.as_str() {
            "workspace list" if exists => ok(WORKSPACES),
            "workspace list" => ok(r#"{"result":{"workspaces":[]}}"#),
            "tab list --workspace w1" => ok(r#"{"result":{"tabs":[{"tab_id":"t9"}]}}"#),
            "pane list --workspace w1" => ok(r#"{"result":{"panes":[{"pane_id":"p9"}]}}"#),
            _ => ok(r#"{"result":{}}"#),
        }
    });
    let host = HerdrHost::with_background(fake.clone(), None);
    assert_eq!(host.start(&launch()).unwrap().pane, "p9");
    let calls = lines(&fake);
    assert!(calls.contains(
        &"herdr workspace create --label idlemax workers --cwd /w/app/fix --env IDLEMAX_JOB_ID=7 --no-focus"
            .to_string()
    ));
    assert!(calls.contains(&"herdr tab rename t9 im: App".to_string()));
}

#[test]
fn a_call_that_times_out_fails_the_start() {
    let fake = Arc::new(FakeCommands::new());
    fake.push_error(CmdError::TimedOut);
    let host = HerdrHost::with_background(fake, None);
    let err = host.start(&launch()).unwrap_err();
    assert!(
        matches!(
            err,
            HostError::Cmd {
                source: CmdError::TimedOut,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn stop_closes_only_a_pane_that_is_still_the_workers() {
    let fake = Arc::new(FakeCommands::new());
    fake.set_fallback(|cmd| match cmd.args.join(" ").as_str() {
        "pane list" => ok(PANES_AFTER),
        _ => ok(r#"{"result":{}}"#),
    });
    let host = HerdrHost::with_background(fake.clone(), None);
    let mine = Handle {
        pane: "p1".into(),
        workspace: "w1".into(),
        label: "im-app-old-1".into(),
    };
    let renamed = Handle {
        label: "im-app-other-2".into(),
        ..mine.clone()
    };
    assert!(matches!(
        host.stop(&renamed),
        Err(HostError::NotOurs { .. })
    ));
    let moved = Handle {
        workspace: "w0".into(),
        ..mine.clone()
    };
    assert!(matches!(host.stop(&moved), Err(HostError::NotOurs { .. })));
    assert!(!lines(&fake).iter().any(|c| c.contains("pane close")));
    host.stop(&mine).unwrap();
    assert!(lines(&fake).contains(&"herdr pane close p1".to_string()));
    let gone = Handle {
        pane: "p7".into(),
        ..mine
    };
    host.stop(&gone).unwrap();
}

#[test]
fn state_reads_status_session_and_pids_and_end_text_keeps_the_tail() {
    let fake = Arc::new(FakeCommands::new());
    fake.set_fallback(|cmd| match cmd.args.join(" ").as_str() {
        "pane list" => ok(PANES_AFTER),
        "agent list" => ok(
            r#"{"result":{"agents":[{"pane_id":"p1","agent_status":"working",
            "agent_session":{"value":"sess-1"},"kind":"claude"}]}}"#,
        ),
        l if l.starts_with("pane process-info") => ok(INFO),
        l if l.starts_with("agent read") => {
            let long = "x".repeat(3000);
            ok(&format!(r#"{{"result":{{"text":"first\n{long}"}}}}"#))
        }
        _ => ok(r#"{"result":{}}"#),
    });
    let host = HerdrHost::with_background(fake, None);
    let handle = Handle {
        pane: "p1".into(),
        workspace: "w1".into(),
        label: "im-app-old-1".into(),
    };
    let state = host.state(&handle).unwrap();
    assert_eq!(state.status, AgentStatus::Working);
    assert_eq!(state.session.as_deref(), Some("sess-1"));
    assert_eq!(
        (state.shell_pid, state.foreground),
        (Some(4242), vec![4300])
    );
    let text = host.end_text(&handle).unwrap();
    assert_eq!(text.chars().count(), MAX_END_TEXT_CHARS);
    let gone = Handle {
        pane: "p8".into(),
        ..handle
    };
    assert_eq!(host.state(&gone).unwrap().status, AgentStatus::Gone);
}

#[test]
fn a_renamed_pane_is_never_prompted_or_read() {
    let fake = Arc::new(FakeCommands::new());
    fake.set_fallback(|cmd| match cmd.args.join(" ").as_str() {
        "pane list" => ok(PANES_AFTER),
        _ => ok(r#"{"result":{"text":"owner's screen"}}"#),
    });
    let host = HerdrHost::with_background(fake.clone(), None);
    let renamed = Handle {
        pane: "p1".into(),
        workspace: "w1".into(),
        label: "im-app-mine-9".into(),
    };
    assert!(matches!(
        host.send(&renamed, "brief"),
        Err(HostError::NotOurs { .. })
    ));
    assert_eq!(host.end_text(&renamed), None);
    assert_eq!(host.state(&renamed).unwrap().status, AgentStatus::Gone);
    assert!(!host.matches(&renamed).unwrap());
    let calls = lines(&fake);
    assert!(!calls
        .iter()
        .any(|c| c.contains("agent prompt") || c.contains("agent read")));
}

#[test]
fn a_failed_agent_start_closes_the_new_pane() {
    let fake = existing_tab();
    let failing = Arc::new(FakeCommands::new());
    let inner = Arc::clone(&fake);
    failing.set_fallback(move |cmd| {
        if cmd.args.first().map(String::as_str) == Some("agent") {
            return Ok(Output {
                code: Some(1),
                stdout: String::new(),
                stderr: "no such agent kind".into(),
            });
        }
        inner.run(cmd)
    });
    let host = HerdrHost::with_background(failing, None);
    let err = host.start(&launch()).unwrap_err();
    assert!(matches!(err, HostError::Failed { .. }), "{err:?}");
    assert!(lines(&fake).contains(&"herdr pane close p2".to_string()));
}

#[test]
fn two_new_panes_fail_the_start_and_both_are_closed() {
    let fake = Arc::new(FakeCommands::new());
    let lists = Arc::new(AtomicUsize::new(0));
    fake.set_fallback(move |cmd| {
        let line = cmd.args.join(" ");
        match line.as_str() {
            "workspace list" => ok(WORKSPACES),
            "tab list --workspace w1" => ok(TABS),
            "pane list --workspace w1" if lists.fetch_add(1, Ordering::SeqCst) == 0 => {
                ok(PANES_BEFORE)
            }
            "pane list --workspace w1" => ok(r#"{"result":{"panes":[
                {"pane_id":"p1","label":"im-app-old-1","workspace_id":"w1","tab_id":"t1"},
                {"pane_id":"p2","workspace_id":"w1"},{"pane_id":"p3","workspace_id":"w1"}]}}"#),
            _ => ok(r#"{"result":{}}"#),
        }
    });
    let host = HerdrHost::with_background(fake.clone(), None);
    assert!(host.start(&launch()).is_err());
    let calls = lines(&fake);
    assert!(calls.contains(&"herdr pane close p2".to_string()));
    assert!(calls.contains(&"herdr pane close p3".to_string()));
    assert!(!calls.contains(&"herdr pane close p1".to_string()));
}

/// A worktree idlemax made for Run 3 under `<temp>/worktrees`, and a launch into `folder`.
fn trust_rig(folder_in: &str) -> (tempfile::TempDir, Launch) {
    let temp = tempfile::tempdir().unwrap();
    let worktrees = temp.path().join("worktrees");
    let folder = temp.path().join(folder_in);
    std::fs::create_dir_all(&folder).unwrap();
    let marker =
        r#"{"run":3,"repo":"/r","branch":"idlemax/x","base_commit":"abc","creation_time":1}"#;
    std::fs::write(format!("{}.json", folder.display()), marker).unwrap();
    std::fs::create_dir_all(&worktrees).unwrap();
    let launch = Launch {
        folder: folder.clone(),
        run: 3,
        worktrees_dir: worktrees,
        ..launch()
    };
    (temp, launch)
}

/// A Herdr whose agent start is stuck at the trust dialog for `folder`.
fn stuck_at_trust(folder: &std::path::Path) -> Arc<FakeCommands> {
    let fake = existing_tab();
    let inner = existing_tab();
    let screen = format!(
        "Accessing workspace:\n\n {}\n\n Quick safety check: Is this a project you created or \
         one you trust?\n ❯ 1. No, exit\n   2. Yes, I trust this folder\n",
        folder.display()
    );
    fake.set_fallback(move |cmd| {
        let line = cmd.args.join(" ");
        match line.as_str() {
            l if l.starts_with("agent start") => Ok(Output {
                code: Some(1),
                stdout: String::new(),
                stderr: "agent_not_ready: blocked during startup".into(),
            }),
            l if l.starts_with("pane read p2") => ok(&screen),
            "agent list" => ok(r#"{"result":{"agents":[{"pane_id":"p2","agent_status":"idle"}]}}"#),
            _ => inner.run(cmd),
        }
    });
    fake
}

#[test]
fn a_start_stuck_at_the_trust_dialog_of_our_worktree_answers_yes_and_goes_on() {
    let (_temp, launch) = trust_rig("worktrees/app/fix");
    let fake = stuck_at_trust(&launch.folder);
    let host = HerdrHost::with_background(fake.clone(), None);
    assert_eq!(host.start(&launch).unwrap().pane, "p2");
    let calls = lines(&fake);
    let down = calls
        .iter()
        .position(|c| c == "herdr pane send-keys p2 Down");
    let enter = calls
        .iter()
        .position(|c| c == "herdr pane send-keys p2 Enter");
    assert!(down.is_some() && down < enter, "{calls:#?}");
    assert!(
        !calls.iter().any(|c| c.starts_with("herdr pane close")),
        "{calls:#?}"
    );
}

#[test]
fn the_trust_dialog_of_a_folder_outside_the_worktrees_or_of_another_run_is_never_answered() {
    // Red team case 9: a marker beside the owner's checkout, or another Run's worktree.
    let (_a, outside) = trust_rig("owner-checkout");
    let (_b, mut other_run) = trust_rig("worktrees/app/other");
    other_run.run = 4;
    for launch in [outside, other_run] {
        let fake = stuck_at_trust(&launch.folder);
        let host = HerdrHost::with_background(fake.clone(), None);
        assert!(host.start(&launch).is_err());
        let calls = lines(&fake);
        assert!(!calls.iter().any(|c| c.contains("send-keys")), "{calls:#?}");
    }
}

#[test]
fn on_linux_the_panes_shell_moves_into_a_worker_scope_before_the_agent_starts() {
    // Red team case 10: what the worker forks stays in its scope, and a stop ends it.
    let fake = existing_tab();
    let host = HerdrHost::with_background(fake.clone(), None)
        .scoped(Some(crate::agent::worker::scope::WorkerScope::for_test()));
    host.start(&launch()).unwrap();
    let calls = lines(&fake);
    let adopt = calls
        .iter()
        .position(|c| c.starts_with("busctl --user call org.freedesktop.systemd1"))
        .unwrap_or_else(|| panic!("no scope call in {calls:#?}"));
    assert!(calls[adopt].contains("StartTransientUnit ssa(sv)a(sa(sv)) idlemax-w-7-1-"));
    assert!(
        calls[adopt].ends_with(".scope fail 2 CollectMode s inactive-or-failed PIDs au 1 4242 0"),
        "{}",
        calls[adopt]
    );
    let start = calls
        .iter()
        .position(|c| c.contains("agent start"))
        .unwrap();
    assert!(adopt < start, "{calls:#?}");
}
