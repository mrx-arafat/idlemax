//! Jobs: plan with the domain `AgentState`, run the actions in the runner, reap exits and
//! queue the resulting updates. Part of the agent event loop (adapter layer).

use idlemax_core::protocol::AgentCommand;
use idlemax_core::reconcile::{AgentAction, TickInput};
use idlemax_core::types::{ControlAction, Job};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::{blocking, Agent};
use crate::agent::runner::Runner;
use crate::state::now_ms;

impl Agent {
    pub(super) async fn on_local_controls(
        &mut self,
        first: ControlAction,
        control: &mut mpsc::UnboundedReceiver<ControlAction>,
    ) {
        let mut controls = vec![first];
        while let Ok(c) = control.try_recv() {
            controls.push(c);
        }
        info!(?controls, "local control");
        self.reconcile(&[], &controls, &[]).await;
    }

    /// Plans with the domain state, runs the actions in the runner, queues the updates.
    pub(super) async fn reconcile(
        &mut self,
        commands: &[AgentCommand],
        local: &[ControlAction],
        assign: &[Job],
    ) {
        let input = TickInput {
            guard: &self.decision,
            kill_switch: self.reading.kill_switch,
            online: self.online,
            commands,
            local_controls: local,
            assign,
            shutting_down: self.stopping,
        };
        let actions = self.state.plan(&input);
        if actions.is_empty() {
            return;
        }
        let outcomes = self.with_runner(move |r| {
            actions
                .into_iter()
                .map(|a| {
                    let outcome = execute(r, &a);
                    (a, outcome)
                })
                .collect::<Vec<_>>()
        });
        let Some(outcomes) = outcomes.await else {
            return;
        };
        let now = now_ms();
        for (action, outcome) in outcomes {
            log_action(&action, &outcome);
            if let Some(u) = self.state.applied(&action, outcome, &self.device.id, now) {
                self.outbox.push(u);
            }
        }
    }

    /// Reaps finished jobs into the outbox.
    pub(super) async fn reap(&mut self) {
        let Some(updates) = self.with_runner(Runner::poll).await else {
            return;
        };
        for u in updates {
            let name = self.state.job_name(u.id).map(str::to_string);
            let u = self.state.finished(u);
            info!(job = u.id, status = %u.status, exit_code = ?u.exit_code, error = ?u.error, "job ended");
            if self.notify {
                let name = name.as_deref().unwrap_or("job");
                crate::agent::notify::notify_job_finish(
                    name,
                    u.status,
                    u.exit_code,
                    u.error.as_deref(),
                );
            }
            self.outbox.push(u);
        }
    }

    /// Runs `f` on the runner off the async threads and refreshes the running list.
    async fn with_runner<R: Send + 'static>(
        &mut self,
        f: impl FnOnce(&mut Runner) -> R + Send + 'static,
    ) -> Option<R> {
        let done = blocking(&self.runner, move |r| {
            let out = f(r);
            (out, r.running(), r.job_dirs())
        });
        match done.await {
            Ok((out, running, job_dirs)) => {
                self.running = running;
                self.job_dirs = job_dirs;
                Some(out)
            }
            Err(e) => {
                error!("runner worker failed: {e}");
                None
            }
        }
    }
}

fn execute(runner: &mut Runner, action: &AgentAction) -> Result<(), String> {
    let done = match action {
        AgentAction::Start(job) => runner.start(job),
        AgentAction::Pause(id) => runner.pause(*id),
        AgentAction::Resume(id) => runner.resume(*id),
        AgentAction::Kill { id, .. } => runner.kill(*id),
        AgentAction::Return { .. } => return Ok(()),
    };
    done.map_err(|e| e.to_string())
}

fn log_action(action: &AgentAction, outcome: &Result<(), String>) {
    let job = action.job_id();
    match (action, outcome) {
        (AgentAction::Start(j), Ok(())) => info!(job, name = %j.spec.name, "job started"),
        (AgentAction::Return { reason, .. }, _) => info!(job, %reason, "lease returned"),
        (AgentAction::Pause(_), Ok(())) => info!(job, "job paused"),
        (AgentAction::Resume(_), Ok(())) => info!(job, "job resumed"),
        (AgentAction::Kill { order, .. }, Ok(())) => {
            warn!(job, reason = %order.reason, owner_stop = order.owner_stop, retryable = order.retryable, "killing job");
        }
        (_, Err(e)) => warn!(job, error = %e, "runner action failed"),
    }
}
