//! Jobs: plan with the domain `AgentState`, run the actions in the runner, reap exits and
//! queue the resulting updates. Part of the agent event loop (adapter layer).

use std::collections::HashMap;

use idlemax_core::dispatch::room::lines_verdict;
use idlemax_core::dispatch::room::LineState;
use idlemax_core::protocol::AgentCommand;
use idlemax_core::reconcile::{AgentAction, TickInput};
use idlemax_core::types::{ControlAction, Job, JobId};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use super::{blocking, Agent};
use crate::agent::api::LocalControl;
use crate::agent::runner::{expand_cwd, Runner};
use crate::agent::worker::Msg;
use crate::state::now_ms;

impl Agent {
    pub(super) async fn on_local_controls(
        &mut self,
        first: LocalControl,
        control: &mut mpsc::UnboundedReceiver<LocalControl>,
    ) {
        let mut controls = vec![first.action];
        let mut acknowledgements = vec![first.applied];
        while let Ok(c) = control.try_recv() {
            controls.push(c.action);
            acknowledgements.push(c.applied);
        }
        info!(?controls, "local control");
        let applied = self.reconcile(&[], &controls, &[]).await;
        self.publish();
        if applied {
            for acknowledgement in acknowledgements {
                let _ = acknowledgement.send(());
            }
        }
    }

    /// Plans with the domain state, runs the actions in the runner, queues the updates.
    pub(super) async fn reconcile(
        &mut self,
        commands: &[AgentCommand],
        local: &[ControlAction],
        assign: &[Job],
    ) -> bool {
        let worker_mem: Vec<(JobId, u64)> = self
            .running
            .iter()
            .filter(|r| self.state.is_worker(r.id))
            .map(|r| (r.id, r.rss_bytes.unwrap_or(0)))
            .collect();
        let pressed = matches!(self.room.state, LineState::Shed | LineState::Emergency);
        let others = match (&self.workers, pressed && !worker_mem.is_empty()) {
            (Some(link), true) => link.others(),
            _ => String::new(),
        };
        let input = TickInput {
            guard: &self.decision,
            kill_switch: self.reading.kill_switch,
            online: self.online,
            commands,
            local_controls: local,
            assign,
            shutting_down: self.stopping,
            lines: &lines_verdict(Some(&self.room), self.room.at_ms),
            room: &self.room,
            worker_mem: &worker_mem,
            others: &others,
        };
        let planned = self.state.plan(&input);
        let (for_workers, actions): (Vec<_>, Vec<_>) =
            planned.into_iter().partition(|a| self.is_worker_action(a));
        self.send_to_workers(for_workers);
        if actions.is_empty() {
            return true;
        }
        let mut checking = HashMap::new();
        let mut resolved = Vec::with_capacity(actions.len());
        for action in actions {
            let cwd = match &action {
                AgentAction::Start(job) => expand_cwd(job.spec.cwd.as_deref())
                    .map(Some)
                    .map_err(|e| e.to_string()),
                _ => Ok(None),
            };
            if let Ok(Some(path)) = &cwd {
                checking.entry(path.clone()).or_insert_with(|| {
                    let checker = self.cwd_check.clone();
                    let path = path.clone();
                    tokio::spawn(async move { checker.check(path).await })
                });
            }
            resolved.push((action, cwd));
        }
        let mut checked = HashMap::with_capacity(checking.len());
        for (path, task) in checking {
            let result = task
                .await
                .map_err(|e| e.to_string())
                .and_then(|answer| answer.map_err(|e| e.to_string()));
            checked.insert(path, result);
        }
        let prepared = resolved
            .into_iter()
            .map(|(action, cwd)| {
                let result = cwd.and_then(|path| match path {
                    Some(path) => checked
                        .get(&path)
                        .cloned()
                        .ok_or_else(|| "cwd check result missing".to_string())?
                        .map(Some),
                    None => Ok(None),
                });
                (action, result)
            })
            .collect::<Vec<_>>();
        let outcomes = self.with_runner(move |r| {
            prepared
                .into_iter()
                .map(|(a, cwd)| {
                    let outcome = cwd.and_then(|cwd| execute(r, &a, cwd.as_deref()));
                    (a, outcome)
                })
                .collect::<Vec<_>>()
        });
        let Some(outcomes) = outcomes.await else {
            return false;
        };
        let now = now_ms();
        let mut applied = true;
        for (action, outcome) in outcomes {
            applied &= outcome.is_ok();
            log_action(&action, &outcome);
            if let Some(u) = self.state.applied(&action, outcome, &self.device.id, now) {
                self.outbox.push(u);
            }
        }
        applied
    }

    /// Reaps finished jobs into the outbox.
    pub(super) async fn reap(&mut self) {
        self.reap_with(Runner::poll).await;
    }

    pub(super) async fn reap_exits(&mut self) {
        self.reap_with(Runner::poll_exits).await;
    }

    async fn reap_with(&mut self, poll: fn(&mut Runner) -> Vec<idlemax_core::protocol::JobUpdate>) {
        let Some(updates) = self.with_runner(poll).await else {
            return;
        };
        self.refill_due |= !updates.is_empty();
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
    pub(super) async fn with_runner<R: Send + 'static>(
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
                self.merge_worker_running();
                Some(out)
            }
            Err(e) => {
                error!("runner worker failed: {e}");
                None
            }
        }
    }
}

impl Agent {
    /// A start of a worker lease, or a pause, resume or kill of a tracked worker.
    fn is_worker_action(&self, action: &AgentAction) -> bool {
        match action {
            AgentAction::Start(job) => job.spec.worker.is_some(),
            AgentAction::Return { .. } => false,
            other => self.state.is_worker(other.job_id()),
        }
    }

    /// Hands worker actions to the manager without waiting for it.
    fn send_to_workers(&mut self, actions: Vec<AgentAction>) {
        let now = now_ms();
        for action in actions {
            let msg = match &action {
                AgentAction::Start(job) => Some(Msg::Start(job.clone())),
                AgentAction::Pause(id) => Some(Msg::Pause(*id)),
                AgentAction::Resume(id) => Some(Msg::Resume(*id)),
                AgentAction::Kill { id, order } => Some(Msg::Kill {
                    id: *id,
                    order: order.clone(),
                }),
                AgentAction::Return { .. } => None,
            };
            let outcome = match (&self.workers, msg) {
                (Some(link), Some(msg)) => {
                    link.send(msg);
                    Ok(())
                }
                (None, Some(_)) => Err(
                    "this agent cannot read its process table, so it runs no worker".to_string(),
                ),
                (_, None) => Ok(()),
            };
            log_action(&action, &outcome);
            if let Some(u) = self.state.applied(&action, outcome, &self.device.id, now) {
                self.outbox.push(u);
            }
        }
    }
}

fn execute(
    runner: &mut Runner,
    action: &AgentAction,
    cwd: Option<&std::path::Path>,
) -> Result<(), String> {
    let done = match action {
        AgentAction::Start(job) => {
            runner.start_in(job, cwd.ok_or("cwd was not checked before start")?)
        }
        AgentAction::Pause(id) => runner.pause(*id),
        AgentAction::Resume(id) => runner.resume(*id),
        AgentAction::Kill { id, order } => runner.kill_ordered(*id, order),
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
