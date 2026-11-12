//! Adapter: the pane phases of a worker launch: start the agent, check its real environment
//! (once more with the found names blanked), and send the brief only while it is `idle`.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;

use idlemax_core::worker::brief::{resume_line, start_line};
use idlemax_core::worker::launch::{env_problems, pane_env};
use idlemax_core::worker::{name, WorkerAction};

use super::super::host::{AgentStatus, Handle, HostState, Launch};
use super::{brief_path, LaunchCtx, Progress, Stop};

/// Starts the agent and checks its real environment; a pane whose environment holds a
/// credential is made once more with those names blanked, and never briefed if they stay.
pub(super) fn start_checked(
    ctx: &LaunchCtx,
    p: &mut Progress,
    folder: &Path,
    remotes: &[String],
    args: &[String],
) -> Result<Handle, Stop> {
    let mut blank = Vec::new();
    for attempt in 0..2 {
        let launch = Launch {
            name: ctx.job.spec.name.clone(),
            tab: name::tab_label(&ctx.worker.project),
            folder: folder.to_path_buf(),
            env: pane_env(ctx.job.id, ctx.tag, remotes, &blank),
            cli: ctx.worker.cli,
            args: args.to_vec(),
            job: ctx.job.id,
            attempt: ctx.job.attempts,
        };
        let handle = ctx
            .host
            .start(&launch)
            .map_err(|e| Stop::Failed(format!("the worker could not be started: {e}")))?;
        p.handle = Some(handle.clone());
        let state = wait_idle(ctx, &handle)?;
        p.shell_pid = state.shell_pid;
        p.session = state.session.clone().or(p.session.take());
        let pid = state.foreground.first().copied().or(state.shell_pid);
        let vars = pid.and_then(|pid| ctx.procs.lock().ok()?.environ(pid));
        let problems = match vars {
            Some(vars) => env_problems(&vars),
            None => {
                return Err(close(
                    ctx,
                    p,
                    "the worker's environment could not be read".into(),
                ))
            }
        };
        if problems.is_empty() {
            return Ok(handle);
        }
        let _ = ctx.host.stop(&handle);
        p.handle = None;
        if attempt == 1 {
            return Err(Stop::Failed(format!(
                "the worker's environment still holds {}; guard each export of it in your shell \
                 start-up file with [ -z \"$IDLEMAX_JOB_ID\" ]",
                problems.join(", ")
            )));
        }
        blank = problems;
    }
    Err(Stop::Failed("the worker could not be started".into()))
}

fn wait_idle(ctx: &LaunchCtx, handle: &Handle) -> Result<HostState, Stop> {
    let since = Instant::now();
    loop {
        if ctx.cancel.load(Ordering::SeqCst) {
            return Err(Stop::Cancelled);
        }
        let state = ctx.host.state(handle).ok();
        match state.as_ref().map(|s| s.status) {
            Some(AgentStatus::Idle) => return state.ok_or(Stop::Cancelled),
            Some(AgentStatus::Gone) => {
                return Err(Stop::Failed("the agent exited before it finished".into()))
            }
            _ if since.elapsed() >= ctx.idle_wait => {
                return Err(Stop::Failed(format!(
                    "the worker did not reach idle within {} s",
                    ctx.idle_wait.as_secs()
                )))
            }
            _ => std::thread::sleep(ctx.poll),
        }
    }
}

/// Closes the pane of a launch that failed and gives the failure.
fn close(ctx: &LaunchCtx, p: &mut Progress, error: String) -> Stop {
    if let Some(handle) = p.handle.take() {
        let _ = ctx.host.stop(&handle);
    }
    Stop::Failed(error)
}

/// Sends the first line once the agent reads `idle` again, so it never lands in a dialog.
pub(super) fn send_brief(ctx: &LaunchCtx, handle: &Handle, folder: &Path) -> Result<(), Stop> {
    wait_idle(ctx, handle)?;
    let line = brief_line(ctx.worker.action, folder);
    ctx.host
        .send(handle, &line)
        .map_err(|e| Stop::Failed(format!("the brief could not be sent: {e}")))
}

/// The line that points the agent at its brief: start, or continue for a `resume`.
pub fn brief_line(action: WorkerAction, folder: &Path) -> String {
    let brief = brief_path(folder).display().to_string();
    match action {
        WorkerAction::Resume => resume_line(&brief),
        _ => start_line(&brief),
    }
}
