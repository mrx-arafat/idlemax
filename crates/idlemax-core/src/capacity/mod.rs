//! Domain: how much of a device's allowance is in use, and the main reason it is not using
//! more. It asks the scheduler's own checks (`device_blockers`, `job_blockers`, `start_budget`,
//! `pick`) and the report's guard labels instead of repeating them, so this answer cannot
//! disagree with what the scheduler does or with `idlemax report`. Pure: no I/O, time is a
//! parameter.

mod accounts;
mod queue;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::explain::guard_now;
use crate::protocol::DeviceView;
use crate::quota::QuotaView;
use crate::report::{GuardState, HoldReason};
use crate::scheduler::{start_budget, SchedContext};
use crate::types::{ControlScope, GuardDecision, Job, UnixMs};

pub use accounts::{accounts, AccountCapacity};

/// What a device is doing with its allowance, as a stable code for scripts and colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityState {
    /// No heartbeat: asleep, or the agent is down.
    #[default]
    Offline,
    /// The guard or a pause keeps new jobs from starting.
    Held,
    /// Every slot or all of the RAM budget is in use: the device works as hard as allowed.
    Full,
    /// Room to spare and work ready: the device leases it on its next heartbeat.
    Starting,
    /// Room to spare, but the smallest ready job needs more RAM than is free.
    TooBig,
    /// Room to spare, but no queued job can run on this device as it is configured.
    CannotRun,
    /// Room to spare, and the queued jobs wait on a retry delay, AI quota or a pause.
    Waiting,
    /// Room to spare and nothing queued.
    Idle,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Capacity {
    pub state: CapacityState,
    /// Jobs holding a slot: running, paused, and leased but not started yet.
    pub slots_used: u32,
    /// Most jobs the device may hold: used plus free. None while the guard keeps new jobs
    /// from starting, because only a guard that lets them start says how many more fit.
    pub slots_allowed: Option<u32>,
    /// RAM budgeted to those jobs (their `mem_mb`), and the most the device may budget.
    pub mem_used_mib: u64,
    pub mem_allowed_mib: Option<u64>,
    /// The main reason the device is not using more, in words.
    pub why: String,
}

/// The coordinator's view at one instant, as `explain` takes it.
#[derive(Debug, Clone, Copy)]
pub struct CapacityInput<'a> {
    pub now_ms: UnixMs,
    /// The scheduler's own input: every active job and every queued job no pause holds.
    pub jobs: &'a [Job],
    /// Queued jobs a pause holds back; the scheduler never sees them.
    pub held: &'a [Job],
    pub paused: &'a [ControlScope],
    pub quota: &'a [QuotaView],
}

/// What each of `devices` is doing with its allowance, as of its last heartbeat and the
/// coordinator's own knowledge of the jobs it has leased since. What holds each queued job on
/// every device alike (retry delay, AI quota, a busy account) is worked out once, not per
/// device, because `GET /v1/status` asks this on every poll under the store lock.
pub fn device_capacities(input: &CapacityInput<'_>, devices: &[DeviceView]) -> Vec<Capacity> {
    let queue = queue::Queue::new(input);
    devices
        .iter()
        .map(|dv| device_capacity(input, &queue, dv))
        .collect()
}

fn device_capacity(
    input: &CapacityInput<'_>,
    queue: &queue::Queue<'_>,
    dv: &DeviceView,
) -> Capacity {
    let (slots_used, mem_used_mib) = in_use(dv, input.jobs);
    let mut out = Capacity {
        slots_used,
        mem_used_mib,
        ..Capacity::default()
    };
    if !dv.online {
        out.why = "offline".into();
        return out;
    }
    let guard = guard_now(dv, input.jobs);
    if let Some(reason) = held_by(dv, &guard) {
        out.state = CapacityState::Held;
        out.why = format!("held: {}", reason.label());
        return out;
    }
    let ctx = SchedContext {
        now_ms: input.now_ms,
        device: &dv.info,
        guard: &guard,
        running: &dv.running,
        quota: input.quota,
        paused: false,
        paused_accounts: queue.paused_accounts(),
    };
    let (free_slots, free_mem) = start_budget(&ctx, input.jobs).unwrap_or_default();
    let room = queue::Room {
        slots: slots_used + free_slots,
        free_slots,
        free_mem_mib: free_mem,
    };
    out.slots_allowed = Some(room.slots);
    out.mem_allowed_mib = Some(mem_used_mib + free_mem);
    let verdict = queue.judge(input, &ctx, &room);
    out.state = verdict.state;
    out.why = verdict.why;
    out
}

/// Jobs holding a slot on the device and the RAM budgeted to them: those it reports, plus
/// those the coordinator leased or saw start after that report.
fn in_use(dv: &DeviceView, jobs: &[Job]) -> (u32, u64) {
    let reported = dv.running.iter().map(|r| r.mem_mb).sum::<u64>();
    let later = jobs
        .iter()
        .filter(|j| j.status.is_active() && j.device.as_deref() == Some(dv.info.id.as_str()))
        .filter(|j| !dv.running.iter().any(|r| r.id == j.id));
    let (n, mem) = later.fold((0usize, 0u64), |(n, m), j| (n + 1, m + j.spec.mem_mb));
    let slots = u32::try_from(dv.running.len() + n).unwrap_or(u32::MAX);
    (slots, reported + mem)
}

/// The main reason new jobs may not start on the device, in the report's vocabulary.
fn held_by(dv: &DeviceView, guard: &GuardDecision) -> Option<HoldReason> {
    let mut state = GuardState::of(guard);
    if dv.paused {
        state = state.with_reason(HoldReason::Paused);
    }
    if dv.local_paused {
        state = state.with_reason(HoldReason::LocalPause);
    }
    state.reasons.first().copied().filter(|_| state.held)
}
