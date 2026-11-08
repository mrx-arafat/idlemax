//! Domain: how `idlemax job add --wait` polls and which exit code it reports.

use std::time::Duration;

use crate::types::JobStatus;

/// Exit code when the job had not finished within `--timeout`, as in timeout(1).
pub const TIMED_OUT_EXIT: u8 = 124;

const FIRST_POLL: Duration = Duration::from_millis(500);
const SLOWEST_POLL: Duration = Duration::from_secs(5);

/// Pause before poll number `polls` (0 is the first): 0.5 s, 1 s, 2 s, 4 s, then every 5 s.
pub fn poll_delay(polls: u32) -> Duration {
    FIRST_POLL
        .saturating_mul(1u32.checked_shl(polls).unwrap_or(u32::MAX))
        .min(SLOWEST_POLL)
}

/// The process exit code that mirrors a finished job: 0 only when it succeeded.
pub fn exit_code(status: JobStatus) -> u8 {
    u8::from(status != JobStatus::Succeeded)
}

#[cfg(test)]
mod tests;
