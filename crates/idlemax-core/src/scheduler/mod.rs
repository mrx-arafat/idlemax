//! Domain: which queued jobs a device may lease now, lease deadlines, and the
//! retry / park policy. Pure: no I/O, time is a parameter.
//!
//! Caller contract (coord api + store):
//! - `jobs` is every non-terminal job in the cluster (queued and active).
//! - `attempts` counts leases: increment it when a job is leased.
//! - Set `lease_expires_ms = lease_deadline(lease_secs, now)` on lease and renew it the same
//!   way on every heartbeat that lists the job, so a paused job keeps its lease while
//!   its agent is alive and an orphaned lease ends `lease_secs` after the last heartbeat.
//! - Persist what `expire_leases` and `apply_update` return as the job's new state.

mod blockers;
mod lease;
mod pick;
mod retry;
#[cfg(test)]
mod tests;
mod update;

pub use blockers::{
    ai_leased_accounts, blocker, blockers, cost_allowed, device_blockers, job_blockers,
    job_blockers_in, spendable_headroom, token_class_pct, Blocker, SchedContext,
};
pub use lease::{expire_leases, lease_deadline};
pub use pick::{pick, pick_lines, start_budget};
pub use retry::{backoff_ms, retry_or_park, BACKOFF_BASE_MS, BACKOFF_MAX_MS};
pub use update::apply_update;
