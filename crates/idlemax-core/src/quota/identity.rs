//! Domain: checks account names against device logins and identifies server-only accounts.
//! Pure: no I/O, no clock. Time is a parameter.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::protocol::DeviceView;
use crate::quota::login::Login;
use crate::quota::{AccountConfig, Provider, QuotaConfig, SAME_RESET_MS, SNAPSHOT_MAX_AGE_MS};
use crate::types::{DeviceProfile, QuotaSnapshot, QuotaWindowKind, UnixMs};

/// Facts about a device's logins and capabilities as known to the coordinator.
#[derive(Debug, Clone, Copy)]
pub struct DeviceLogins<'a> {
    pub device: &'a str,
    pub profile: DeviceProfile,
    pub accounts: &'a [String],
    pub has_claude: bool,
    pub claude_login: Option<&'a Login>,
}

/// The login facts of every device row the coordinator holds.
pub fn device_logins(devices: &[DeviceView]) -> Vec<DeviceLogins<'_>> {
    devices
        .iter()
        .map(|d| DeviceLogins {
            device: &d.info.id,
            profile: d.info.profile,
            accounts: &d.info.ai_accounts,
            has_claude: d.info.capabilities.iter().any(|c| c == "claude"),
            claude_login: d.info.claude_login.as_ref(),
        })
        .collect()
}

fn is_laptop(profile: DeviceProfile) -> bool {
    profile == DeviceProfile::Laptop
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    NotLoggedIn,
    LoginUnknown,
    SplitName,
    OneLoginTwoNames,
    SameReading,
    OwnerMayUse,
}

impl ProblemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProblemKind::NotLoggedIn => "not_logged_in",
            ProblemKind::LoginUnknown => "login_unknown",
            ProblemKind::SplitName => "split_name",
            ProblemKind::OneLoginTwoNames => "one_login_two_names",
            ProblemKind::SameReading => "same_reading",
            ProblemKind::OwnerMayUse => "owner_may_use",
        }
    }
}

impl AccountProblem {
    /// The account names this problem closes somewhere; empty for one that only warns.
    pub fn closed_names(&self) -> &[String] {
        match self.kind {
            ProblemKind::NotLoggedIn | ProblemKind::LoginUnknown | ProblemKind::SplitName => {
                &self.accounts
            }
            ProblemKind::OneLoginTwoNames => self.accounts.get(1..).unwrap_or_default(),
            ProblemKind::SameReading | ProblemKind::OwnerMayUse => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProblem {
    pub kind: ProblemKind,
    pub accounts: Vec<String>,
    pub devices: Vec<String>,
    pub message: String,
    pub fix: String,
}

/// Evaluates account problems across configured accounts and reported device logins.
pub fn account_problems(
    cfg: &QuotaConfig,
    devices: &[DeviceLogins<'_>],
    weekly: &[QuotaSnapshot],
    now_ms: UnixMs,
) -> Vec<AccountProblem> {
    let mut problems = Vec::new();

    // 1. Claude device-level closing rules (NotLoggedIn, LoginUnknown) and SplitName
    let mut claude_account_logins: BTreeMap<String, Option<String>> = BTreeMap::new();

    for account in &cfg.accounts {
        if account.provider != Provider::Claude {
            continue;
        }

        let listing: Vec<&DeviceLogins<'_>> = devices
            .iter()
            .filter(|d| d.accounts.iter().any(|a| a == &account.name))
            .collect();

        let mut login_ids = BTreeSet::new();
        let mut split_devices = Vec::new();

        for d in &listing {
            match d.claude_login {
                Some(Login::Out) => {
                    problems.push(AccountProblem {
                        kind: ProblemKind::NotLoggedIn,
                        accounts: vec![account.name.clone()],
                        devices: vec![d.device.to_string()],
                        message: format!(
                            "device {} lists account {} but is not logged in to Claude",
                            d.device, account.name
                        ),
                        fix: format!("run claude auth login on {}", d.device),
                    });
                }
                Some(Login::Unknown { .. }) | Some(Login::In { id: None }) | None => {
                    problems.push(AccountProblem {
                        kind: ProblemKind::LoginUnknown,
                        accounts: vec![account.name.clone()],
                        devices: vec![d.device.to_string()],
                        message: format!(
                            "cannot tell whether device {} is logged in to Claude",
                            d.device
                        ),
                        fix: format!("run idlemax doctor on {}", d.device),
                    });
                }
                Some(Login::In { id: Some(id) }) => {
                    login_ids.insert(id.clone());
                    split_devices.push(d.device.to_string());
                }
            }
        }

        if login_ids.len() > 1 {
            split_devices.sort();
            split_devices.dedup();
            problems.push(AccountProblem {
                kind: ProblemKind::SplitName,
                accounts: vec![account.name.clone()],
                devices: split_devices,
                message: format!(
                    "account {} has different Claude logins across devices",
                    account.name
                ),
                fix: "give one login its own [[quota.accounts]] entry and list that name in the device's agent.ai_accounts".to_string(),
            });
        } else if login_ids.len() == 1 {
            claude_account_logins.insert(account.name.clone(), login_ids.into_iter().next());
        }
    }

    // 2. OneLoginTwoNames across Claude accounts
    let mut login_to_accounts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for account in &cfg.accounts {
        if let Some(Some(login_id)) = claude_account_logins.get(&account.name) {
            login_to_accounts
                .entry(login_id.clone())
                .or_default()
                .push(account.name.clone());
        }
    }

    for (login_id, accounts) in login_to_accounts {
        if accounts.len() > 1 {
            let mut affected_devices = Vec::new();
            for acc in &accounts {
                for d in devices {
                    if d.accounts.iter().any(|a| a == acc) {
                        affected_devices.push(d.device.to_string());
                    }
                }
            }
            affected_devices.sort();
            affected_devices.dedup();

            problems.push(AccountProblem {
                kind: ProblemKind::OneLoginTwoNames,
                accounts,
                devices: affected_devices,
                message: format!(
                    "Claude login {} is used by multiple account names",
                    login_id
                ),
                fix: "one name on every device".to_string(),
            });
        }
    }

    // 3. SameReading for Codex
    check_codex_same_reading(cfg, weekly, now_ms, &mut problems);

    // 4. OwnerMayUse warning for Claude accounts that could be server-only
    check_owner_may_use(cfg, devices, &mut problems);

    problems
}

fn check_codex_same_reading(
    cfg: &QuotaConfig,
    weekly: &[QuotaSnapshot],
    now_ms: UnixMs,
    problems: &mut Vec<AccountProblem>,
) {
    let codex_accounts: Vec<&AccountConfig> = cfg
        .accounts
        .iter()
        .filter(|a| a.provider == Provider::Codex)
        .collect();

    // Map each codex account to its newest weekly snapshot
    let mut newest_snaps: Vec<(&str, &QuotaSnapshot)> = Vec::new();
    for acc in &codex_accounts {
        let newest = weekly
            .iter()
            .filter(|s| {
                s.account == acc.name
                    && s.window == QuotaWindowKind::Weekly
                    && s.resets_at_ms.is_some()
                    && now_ms.saturating_sub(s.at_ms) <= SNAPSHOT_MAX_AGE_MS
            })
            .max_by_key(|s| s.at_ms);

        if let Some(snap) = newest {
            newest_snaps.push((&acc.name, snap));
        }
    }

    for i in 0..newest_snaps.len() {
        for j in (i + 1)..newest_snaps.len() {
            let (acc1, snap1) = newest_snaps[i];
            let (acc2, snap2) = newest_snaps[j];

            let reset1 = snap1.resets_at_ms.unwrap_or(0);
            let reset2 = snap2.resets_at_ms.unwrap_or(0);

            if (reset1 - reset2).abs() <= SAME_RESET_MS
                && (snap1.used_pct - snap2.used_pct).abs() <= 1.0
            {
                problems.push(AccountProblem {
                    kind: ProblemKind::SameReading,
                    accounts: vec![acc1.to_string(), acc2.to_string()],
                    devices: Vec::new(),
                    message: format!(
                        "codex accounts {} and {} have weekly resets within 5 minutes and readings within 1.0%; they may be one login",
                        acc1, acc2
                    ),
                    fix: "one name on both devices shares the budget".to_string(),
                });
            }
        }
    }
}

fn check_owner_may_use(
    cfg: &QuotaConfig,
    devices: &[DeviceLogins<'_>],
    problems: &mut Vec<AccountProblem>,
) {
    for account in &cfg.accounts {
        if account.provider != Provider::Claude {
            continue;
        }

        let listing: Vec<&DeviceLogins<'_>> = devices
            .iter()
            .filter(|d| d.accounts.iter().any(|a| a == &account.name))
            .collect();

        if listing.is_empty() || listing.iter().any(|d| is_laptop(d.profile)) {
            continue;
        }

        let mut listing_ids = BTreeSet::new();
        for d in &listing {
            if let Some(Login::In { id: Some(ref id) }) = d.claude_login {
                listing_ids.insert(id.clone());
            } else {
                listing_ids.clear();
                break;
            }
        }
        if listing_ids.len() != 1 {
            continue;
        }
        let Some(login_id) = listing_ids.into_iter().next() else {
            continue;
        };

        // Check if a laptop reports the same login id explicitly
        let shares_id = devices.iter().any(|d| {
            is_laptop(d.profile)
                && matches!(d.claude_login, Some(Login::In { id: Some(ref id) }) if id == &login_id)
        });
        if shares_id {
            continue;
        }

        // Check if any laptop has Claude with unknown or missing login
        for d in devices.iter().filter(|d| is_laptop(d.profile)) {
            if d.has_claude {
                match d.claude_login {
                    None | Some(Login::Unknown { .. }) | Some(Login::In { id: None }) => {
                        problems.push(AccountProblem {
                            kind: ProblemKind::OwnerMayUse,
                            accounts: vec![account.name.clone()],
                            devices: vec![d.device.to_string()],
                            message: format!(
                                "cannot tell whether laptop {} is on the same Claude login as server-only account {}",
                                d.device, account.name
                            ),
                            fix: "add a [[quota.accounts]] entry for the laptop's own Claude account to its config.toml, so its agent reports the login".to_string(),
                        });
                    }
                    Some(Login::Out) | Some(Login::In { id: Some(_) }) => {}
                }
            }
        }
    }
}

/// Checks whether an account may be used on a device under the discovered account problems.
pub fn usable_on(problems: &[AccountProblem], device: &str, account: &str) -> Result<(), String> {
    for p in problems {
        match p.kind {
            ProblemKind::NotLoggedIn | ProblemKind::LoginUnknown => {
                if p.accounts.iter().any(|a| a == account) && p.devices.iter().any(|d| d == device)
                {
                    return Err(format!("{}: {}", p.message, p.fix));
                }
            }
            ProblemKind::SplitName => {
                if p.accounts.iter().any(|a| a == account) {
                    return Err(format!("{}: {}", p.message, p.fix));
                }
            }
            ProblemKind::OneLoginTwoNames => {
                // Closes every name but the first in [[quota.accounts]] order
                if p.accounts.len() > 1 && p.accounts[1..].iter().any(|a| a == account) {
                    return Err(format!("{}: {}", p.message, p.fix));
                }
            }
            ProblemKind::SameReading | ProblemKind::OwnerMayUse => {
                // Warnings only, do not close work
            }
        }
    }
    Ok(())
}

/// Returns true when an account is used exclusively by servers.
pub fn server_only(cfg: &QuotaConfig, account: &str, devices: &[DeviceLogins<'_>]) -> bool {
    let Some(acc) = cfg.accounts.iter().find(|a| a.name == account) else {
        return false;
    };

    let listing: Vec<&DeviceLogins<'_>> = devices
        .iter()
        .filter(|d| d.accounts.iter().any(|a| a == account))
        .collect();

    // Needs at least one known device that lists the account
    if listing.is_empty() {
        return false;
    }

    // No laptop among listing devices
    if listing.iter().any(|d| is_laptop(d.profile)) {
        return false;
    }

    acc.provider != Provider::Claude || claude_login_is_server_only(&listing, devices)
}

/// The listing devices share one known Claude login, and no laptop is or may be on it.
fn claude_login_is_server_only(
    listing: &[&DeviceLogins<'_>],
    devices: &[DeviceLogins<'_>],
) -> bool {
    let mut ids = BTreeSet::new();
    for d in listing {
        match d.claude_login {
            Some(Login::In { id: Some(id) }) => {
                ids.insert(id.as_str());
            }
            _ => return false,
        }
    }
    let (Some(&login_id), 1) = (ids.iter().next(), ids.len()) else {
        return false;
    };
    devices.iter().filter(|d| is_laptop(d.profile)).all(|d| {
        match d.claude_login {
            // The laptop is on the same login.
            Some(Login::In { id: Some(id) }) => id != login_id,
            // A laptop with claude that cannot say which login it holds may be on it.
            None | Some(Login::Unknown { .. }) | Some(Login::In { id: None }) => !d.has_claude,
            Some(Login::Out) => true,
        }
    })
}

#[cfg(test)]
mod tests;
