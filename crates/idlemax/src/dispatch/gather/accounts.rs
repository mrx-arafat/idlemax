//! The accounts each device may use for dispatcher work: the ones its logins do not close
//! (Spec 5 Task 6). Adapter over `quota::identity`.

use idlemax_core::protocol::DeviceView;
use idlemax_core::quota::identity::AccountProblem;
use idlemax_core::quota::{account_problems, device_logins, usable_on};
use idlemax_core::types::UnixMs;
use idlemax_core::Config;

use crate::coord::api::offline_after_ms;
use crate::coord::store::{Store, StoreError};

/// The device rows with every account that `usable_on` refuses (an account that is not
/// logged in, whose login is unknown or split, or one login under two names) taken out of
/// that device's `ai_accounts`, so `build` and `pass_due` never pick it there, and one line
/// per pair with the message and the fix.
pub(super) fn open_accounts(
    store: &Store,
    cfg: &Config,
    now: UnixMs,
) -> Result<OpenAccounts, StoreError> {
    let mut devices = store.devices(now, offline_after_ms(cfg))?;
    let problems = account_problems(
        &cfg.quota,
        &device_logins(&devices),
        &store.latest_quota()?,
        now,
    );
    let mut notes = Vec::new();
    for dv in &mut devices {
        let id = dv.info.id.clone();
        dv.info
            .ai_accounts
            .retain(|account| match usable_on(&problems, &id, account) {
                Ok(()) => true,
                Err(why) => {
                    notes.push(format!("{id}: {account} is closed: {why}"));
                    false
                }
            });
    }
    Ok(OpenAccounts {
        devices,
        notes,
        problems,
    })
}

/// The devices with their closed accounts taken out, a note per closed pair, and the
/// problems that closed them.
pub(super) struct OpenAccounts {
    pub devices: Vec<DeviceView>,
    pub notes: Vec<String>,
    pub problems: Vec<AccountProblem>,
}
