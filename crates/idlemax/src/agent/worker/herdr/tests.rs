//! Tests: the Herdr host builds its calls from fixture JSON in the observed shapes, through
//! `FakeCommands`; no test starts `herdr`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use idlemax_core::worker::WorkerCli;

use super::*;
use crate::agent::worker::cmd::{CmdError, FakeCommands, Output};

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
            r#"{"result":{"agents":[{"pane_id":"p2","agent_status":"working",
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
        pane: "p2".into(),
        workspace: "w1".into(),
        label: "x".into(),
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
