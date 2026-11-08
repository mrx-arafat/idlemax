//! Bounded blocking calls for the agent event loop (adapter layer).
//!
//! A probe can hang for good, for example a stat on a dead network mount. The loop awaits
//! each sample, so one stuck probe would stop the reaper, the kill switch, the local
//! controls and shutdown. These calls give up waiting, and never queue behind a call that
//! is still stuck, so the caller can fall back to a fail-safe verdict.

use std::sync::{Arc, Mutex, TryLockError};
use std::time::Duration;

use tokio::task::JoinError;

#[derive(Debug, thiserror::Error)]
pub(super) enum BoundedError {
    #[error("an earlier call is still stuck")]
    Stuck,
    #[error("no answer within {}s", .0.as_secs())]
    TimedOut(Duration),
    #[error("worker failed: {0}")]
    Worker(#[from] JoinError),
}

/// Runs `f` on a blocking thread like `blocking`, but stops waiting after `limit`. The
/// thread cannot be cancelled, so a call that never returns keeps `cell` locked; later
/// calls then fail at once with `Stuck` until it does.
pub(super) async fn blocking_within<T, R>(
    cell: &Arc<Mutex<T>>,
    limit: Duration,
    f: impl FnOnce(&mut T) -> R + Send + 'static,
) -> Result<R, BoundedError>
where
    T: Send + 'static,
    R: Send + 'static,
{
    let cell = Arc::clone(cell);
    let task = tokio::task::spawn_blocking(move || {
        let mut value = match cell.try_lock() {
            Ok(value) => value,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        Some(f(&mut value))
    });
    match tokio::time::timeout(limit, task).await {
        Ok(joined) => joined?.ok_or(BoundedError::Stuck),
        Err(_) => Err(BoundedError::TimedOut(limit)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[tokio::test]
    async fn a_call_that_never_returns_fails_fast_and_does_not_block_later_calls() {
        let cell = Arc::new(Mutex::new(0u32));
        let limit = Duration::from_millis(100);
        let (release, held) = mpsc::channel::<()>();

        let first = blocking_within(&cell, limit, move |n| {
            let _ = held.recv();
            *n += 1;
        })
        .await;
        assert!(matches!(first, Err(BoundedError::TimedOut(_))), "{first:?}");

        let second = blocking_within(&cell, limit, |n| *n).await;
        assert!(matches!(second, Err(BoundedError::Stuck)), "{second:?}");

        release.send(()).unwrap();
        let mut third = blocking_within(&cell, limit, |n| *n).await;
        for _ in 0..50 {
            if third.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            third = blocking_within(&cell, limit, |n| *n).await;
        }
        assert_eq!(third.unwrap(), 1, "the stuck call finished and unlocked");
    }
}
