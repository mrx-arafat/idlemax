//! Adapter: stopping a job. A cap breach or a kill request sends SIGTERM to the job's group
//! and its outside members; SIGKILL follows when the grace period passes.

use std::io;

use idlemax_core::job_run::{self, StopReason};
use tracing::warn;

use super::os::{self, Sig};
use super::Slot;

pub(super) struct Stop {
    pub(super) reason: StopReason,
    pub(super) term_sent_awake_ms: i64,
    pub(super) kill_sent: bool,
}

/// Escalates a stop in progress, or stops a job that has breached its caps.
pub(super) fn enforce(slot: &mut Slot, awake_ms: i64) {
    let id = slot.job.id;
    if let Some(stop) = slot.stop.as_mut() {
        if !stop.kill_sent && job_run::kill_due(stop.term_sent_awake_ms, awake_ms) {
            match os::signal_job(slot.pid, &slot.outside, Sig::Kill) {
                Ok(()) => stop.kill_sent = true,
                Err(e) => warn!(job = id, error = %e, "cannot SIGKILL job"),
            }
        }
        return;
    }
    let rss = slot.usage.map(|u| u.rss_bytes);
    if let Some(reason) = job_run::breach(&slot.job.spec, slot.clock.active_ms(awake_ms), rss) {
        warn!(job = id, reason = ?reason.message(), "stopping job over its cap");
        if let Err(e) = stop(slot, reason, awake_ms) {
            warn!(job = id, error = %e, "cannot stop job");
        }
    }
}

/// SIGTERM the job (SIGCONT too if paused) and remember that it is stopping.
pub(super) fn stop(slot: &mut Slot, reason: StopReason, awake_ms: i64) -> io::Result<()> {
    if slot.stop.is_some() || slot.child.try_wait()?.is_some() {
        return Ok(());
    }
    os::signal_job(slot.pid, &slot.outside, Sig::Term)?;
    slot.stop = Some(Stop {
        reason,
        term_sent_awake_ms: awake_ms,
        kill_sent: false,
    });
    if slot.clock.is_paused() {
        // A stopped process acts on SIGTERM only once continued.
        os::signal_job(slot.pid, &slot.outside, Sig::Cont)?;
        slot.clock.resume(awake_ms);
    }
    Ok(())
}
