//! Local controls and coordinator commands: they set holds and collect the kill orders one
//! tick will carry out. Part of the agent tick decisions (domain layer, pure).

use std::collections::BTreeMap;

use super::{AgentState, KillOrder};
use crate::protocol::AgentCommand;
use crate::types::{ControlAction, JobId};

type Kills = BTreeMap<JobId, KillOrder>;

impl AgentState {
    pub(super) fn apply_local_controls(&mut self, controls: &[ControlAction], kills: &mut Kills) {
        for c in controls {
            match c {
                ControlAction::Pause => self.local_paused = true,
                ControlAction::Resume => self.local_paused = false,
                ControlAction::StopAll => {
                    self.local_paused = true;
                    self.kill_all(&KillOrder::new("stop_all from local control", true), kills);
                }
                // Not a Global-scope action; the local API rejects it.
                ControlAction::Cancel => {}
            }
        }
    }

    /// Returns true when the coordinator sent StopAll.
    pub(super) fn apply_commands(&mut self, commands: &[AgentCommand], kills: &mut Kills) -> bool {
        let mut stop_all = false;
        for c in commands {
            match c {
                AgentCommand::PauseJob { id } => self.set_command_hold(*id, true),
                AgentCommand::ResumeJob { id } => self.set_command_hold(*id, false),
                AgentCommand::KillJob { id } if self.jobs.contains_key(id) => {
                    kills
                        .entry(*id)
                        .or_insert_with(|| KillOrder::new("killed by coordinator", false));
                }
                AgentCommand::KillJob { .. } => {}
                AgentCommand::StopAll => {
                    stop_all = true;
                    let order = KillOrder::new("stop_all from coordinator", false);
                    self.kill_all(&order, kills);
                }
                AgentCommand::ResumeAll => {
                    self.jobs.values_mut().for_each(|j| j.holds.command = false);
                }
            }
        }
        stop_all
    }

    fn set_command_hold(&mut self, id: JobId, hold: bool) {
        if let Some(job) = self.jobs.get_mut(&id) {
            job.holds.command = hold;
        }
    }

    pub(super) fn kill_all(&self, order: &KillOrder, kills: &mut Kills) {
        for id in self.jobs.keys() {
            kills.entry(*id).or_insert_with(|| order.clone());
        }
    }
}
