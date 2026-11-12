//! Adapter: what one device's launch step reads from the store once, and the pure
//! `worker::pace::may_launch` asked with it.

use idlemax_core::control::paused_accounts;
use idlemax_core::dispatch::accounts::AccountUse;
use idlemax_core::dispatch::plan::DevicePlan;
use idlemax_core::quota::spend::Gate;
use idlemax_core::quota::Provider;
use idlemax_core::types::{JobId, UnixMs};
use idlemax_core::worker::pace::{may_launch, LaunchGate};
use idlemax_core::worker::run::Run;

use super::job::{cli_of, secs_ms};
use super::LaunchStep;
use crate::coord::api::{offline_after_ms, quota_views, Gates};
use crate::coord::store::{Store, StoreError};
use crate::dispatch::Gathered;

/// What one device's launch reads once: its Runs, the jobs it reports, the paused accounts
/// and the Budgets, so a launch on the previous device already counts (Spec 3 Task 6).
pub(super) struct DeviceFacts {
    pub(super) runs: Vec<Run>,
    pub(super) device_runs: Vec<Run>,
    reported: Vec<i64>,
    paused: Vec<String>,
    last_launch_ms: Option<UnixMs>,
    pub(super) gates: Gates,
}

impl DeviceFacts {
    pub(super) fn read(
        store: &Store,
        step: &LaunchStep<'_>,
        device: &str,
    ) -> Result<Self, StoreError> {
        let runs = store.list_runs(None, None, 0)?;
        let device_runs = runs
            .iter()
            .filter(|r| r.device == device)
            .cloned()
            .collect();
        let views = store.devices(step.now, offline_after_ms(step.cfg))?;
        let reported = views
            .iter()
            .find(|d| d.info.id == device)
            .map(|d| d.running.iter().map(|r| r.id).collect())
            .unwrap_or_default();
        let quota = quota_views(store, step.cfg, step.now)?;
        Ok(Self {
            runs,
            device_runs,
            reported,
            paused: paused_accounts(&store.paused_scopes()?),
            last_launch_ms: store.last_worker_launch(device)?,
            gates: Gates::read(store, step.cfg, step.live, &quota, step.now)?,
        })
    }

    /// The plan's line for the account, with `usable` and `why` answered by the worker gate.
    pub(super) fn account(&self, g: &Gathered, account: &str, verdict: Gate) -> Option<AccountUse> {
        let mut line = g
            .plan
            .accounts
            .iter()
            .find(|a| a.account == account)?
            .clone();
        line.usable = verdict.allowed;
        if !verdict.allowed {
            line.why = verdict.reason;
        }
        Some(line)
    }

    pub(super) fn may_launch(
        &self,
        step: &LaunchStep<'_>,
        device: &DevicePlan,
        provider: Provider,
        account: Option<&AccountUse>,
        previous: Option<JobId>,
    ) -> Option<String> {
        let workers = &step.live.workers;
        let name = account.map_or("", |a| a.account.as_str());
        may_launch(
            &LaunchGate {
                now_ms: step.now,
                held: device.held.as_deref(),
                account_paused: self.paused.iter().any(|p| p == name),
                room: &device.room,
                device_runs: &self.device_runs,
                last_launch_ms: self.last_launch_ms,
                settle_ms: secs_ms(workers.settle_secs),
                reported: &self.reported,
                cli_allowed: cli_of(provider)
                    .is_some_and(|c| workers.clis.iter().any(|w| w == c.as_str())),
                account,
            },
            previous,
        )
    }
}
