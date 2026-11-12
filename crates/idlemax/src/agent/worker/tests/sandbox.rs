//! Tests: the worker sandbox of A13 in the manager: a required sandbox that cannot run
//! refuses the launch, `auto` runs bare with a note, and a wrapped launch reaches the CLI
//! through bwrap in both hosts and fails closed when the CLI is not in another mount
//! namespace. A stub stands in for bwrap; no test runs bwrap itself.

use std::os::unix::fs::PermissionsExt;
use std::sync::OnceLock;

use idlemax_core::config::WorkerHostChoice;
use idlemax_core::worker::sandbox::{SandboxFact, SandboxSetting};

use super::*;

/// A stub bwrap that runs what follows the relay's `$0` (the CLI and its arguments), never
/// making a namespace.
fn wrapper() -> Result<PathBuf, String> {
    static WRAPPER: OnceLock<PathBuf> = OnceLock::new();
    Ok(WRAPPER
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap().keep();
            let path = dir.join("bwrap");
            // After `--` come `/bin/sh -c <script> <$0>`, then the CLI and its arguments.
            let body = "#!/bin/sh\nwhile [ $# -gt 0 ] && [ \"$1\" != -- ]; do shift; done\n\
                        shift 5\nexec \"$@\"\n";
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        })
        .clone())
}

fn no_bwrap() -> Result<PathBuf, String> {
    Err("bwrap is not installed".into())
}

fn sandboxed(rig: &mut Rig, setting: SandboxSetting, probe: super::super::choose::SandboxProbe) {
    rig.workers.set_sandbox_policy(setting, Default::default());
    rig.workers.policy.probe = Some(probe);
}

#[test]
fn a_required_sandbox_that_cannot_run_refuses_the_launch() {
    let mut rig = rig();
    sandboxed(&mut rig, SandboxSetting::Required, no_bwrap);
    let first = rig
        .workers
        .handle(Msg::Start(Box::new(job(&rig, WorkerAction::Launch))), 1, 1);
    assert_eq!(first[0].status, JobStatus::Failed);
    let why = first[0].error.clone().unwrap();
    assert!(why.contains("bwrap is not installed"), "{why}");
    assert!(rig.host.starts.lock().unwrap().is_empty());
}

#[test]
fn auto_without_bwrap_runs_with_the_soft_fences_and_says_so() {
    let mut rig = rig();
    sandboxed(&mut rig, SandboxSetting::Auto, no_bwrap);
    for status in [AgentStatus::Idle, AgentStatus::Idle, AgentStatus::Done] {
        rig.host.push_state(status, None, &[SHELL, AGENT]);
    }
    let mut now = 1_000;
    let first = rig.workers.handle(
        Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
        now,
        now,
    );
    // The coordinator drops `error` of a running update, so the note is a Run fact (finding 8).
    assert_eq!(first[0].error, None);
    let fact = first[0].worker.as_ref().unwrap().sandbox.clone();
    let why = "no sandbox, soft fences only: bwrap is not installed";
    assert_eq!(fact, Some(SandboxFact::Bare { why: why.into() }));
    let updates = drive(&mut rig, &mut now, terminal);
    assert!(briefed(&updates), "{updates:#?}");
    let every = updates.iter().filter_map(|u| u.worker.as_ref());
    assert!(
        every.into_iter().all(|w| w.sandbox == fact),
        "each update says it"
    );
}

#[test]
fn a_pane_gets_the_shim_first_on_path_and_fails_closed_when_it_did_not_take() {
    let mut rig = rig();
    sandboxed(&mut rig, SandboxSetting::Auto, wrapper);
    for _ in 0..3 {
        rig.host
            .push_state(AgentStatus::Idle, None, &[SHELL, AGENT]);
    }
    let mut now = 1_000;
    rig.workers.handle(
        Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
        now,
        now,
    );
    let updates = drive(&mut rig, &mut now, terminal);
    let why = last(&updates).error.clone().unwrap_or_default();
    assert!(why.contains("the sandbox did not take"), "{why}");
    assert!(rig.host.sent.lock().unwrap().is_empty(), "never briefed");

    let start = rig.host.starts.lock().unwrap()[0].clone();
    assert!(start.wrap.is_none(), "a pane is wrapped through its PATH");
    let path = &start.env.iter().find(|(k, _)| k == "PATH").unwrap().1;
    let shim_dir = PathBuf::from(path.split(':').next().unwrap());
    assert!(
        shim_dir.starts_with(rig.paths.root().join("sandbox/shims")),
        "{path}"
    );
    let shim = std::fs::read_to_string(shim_dir.join("claude")).unwrap();
    let bwrap = wrapper().unwrap().display().to_string();
    assert!(
        shim.contains(&format!("exec '{bwrap}' '--ro-bind' '/' '/'")),
        "{shim}"
    );
    // A pane gets its own session too, so the CLI cannot push keys into the pane (TIOCSTI).
    assert!(shim.contains("'--new-session'"), "{shim}");
    assert!(
        shim.contains("'--unshare-net'") && shim.ends_with("\"$cli\" \"$@\"\n"),
        "{shim}"
    );
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "the mount namespace check reads /proc")]
fn a_headless_start_through_a_wrapper_that_makes_no_namespace_fails_closed() {
    let mut rig = super::headless::headless_rig("cat >/dev/null");
    rig.workers
        .set_host_policy(WorkerHostChoice::Headless, None);
    sandboxed(&mut rig, SandboxSetting::Required, wrapper);
    let mut now = 1_000;
    rig.workers.handle(
        Msg::Start(Box::new(job(&rig, WorkerAction::Launch))),
        now,
        now,
    );
    let updates = drive(&mut rig, &mut now, terminal);
    let why = last(&updates).error.clone().unwrap_or_default();
    assert!(why.contains("the sandbox did not take"), "{why}");
}

#[test]
fn a_sandbox_the_owner_switched_off_is_said_on_the_run_too() {
    let mut rig = rig();
    sandboxed(&mut rig, SandboxSetting::Off, no_bwrap);
    let first = rig
        .workers
        .handle(Msg::Start(Box::new(job(&rig, WorkerAction::Launch))), 1, 1);
    let fact = first[0].worker.as_ref().unwrap().sandbox.clone();
    assert_eq!(fact, Some(SandboxFact::Off), "review finding 10");
}
