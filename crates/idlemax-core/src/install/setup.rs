//! Domain: what `idlemax setup` does for a role, decided from the flags and from what is
//! already on disk. Pure: the CLI reads the facts, performs the steps and reports each one.

use serde::Serialize;

use super::service::{Platform, ServiceKind};
use crate::config::Config;
use crate::types::{DeviceProfile, Os};

/// What this device is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Runs jobs and reports to a coordinator elsewhere.
    Agent,
    /// Holds the queue; runs no jobs itself.
    Coord,
    /// Both on one device: a complete setup for a single machine.
    Both,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Agent => "agent",
            Role::Coord => "coord",
            Role::Both => "both",
        }
    }

    /// Services to install, in start order: the coordinator first, so the agent finds it.
    pub fn services(self) -> &'static [ServiceKind] {
        match self {
            Role::Agent => &[ServiceKind::Agent],
            Role::Coord => &[ServiceKind::Coord],
            Role::Both => &[ServiceKind::Coord, ServiceKind::Agent],
        }
    }

    pub fn runs_agent(self) -> bool {
        self != Role::Coord
    }

    pub fn runs_coordinator(self) -> bool {
        self != Role::Agent
    }
}

/// What the user asked for.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub role: Role,
    pub os: Os,
    /// `--coord-url`.
    pub coord_url: Option<&'a str>,
    /// `--token-stdin`.
    pub token_given: bool,
    /// False for `--no-service`.
    pub service: bool,
    pub statusline: bool,
}

/// What is already on disk.
#[derive(Debug, Clone, Copy)]
pub struct Existing<'a> {
    /// The parsed `config.toml`, if there is one.
    pub config: Option<&'a Config>,
    pub token: bool,
}

#[derive(Debug, Clone)]
pub enum ConfigPlan {
    /// Create `config.toml` with this content.
    Write(Box<Config>),
    /// Leave the existing file alone.
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenPlan {
    Generate,
    FromStdin,
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatuslinePlan {
    Install,
    Skip(&'static str),
}

#[derive(Debug, Clone)]
pub struct SetupPlan {
    pub role: Role,
    pub config: ConfigPlan,
    pub token: TokenPlan,
    /// The coordinator address this setup ends up with.
    pub coord_url: String,
    /// Services to install, in order; empty when `service_skipped` says why.
    pub services: Vec<ServiceKind>,
    pub service_skipped: Option<&'static str>,
    pub statusline: StatuslinePlan,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupError {
    #[error(
        "--coord-url {0:?} is not an http:// or https:// address, e.g. http://100.64.0.1:7878"
    )]
    BadUrl(String),
    #[error(
        "role agent needs the coordinator's address: pass --coord-url http://<coordinator>:7878 \
         (to run the coordinator on this device too, use --role both)"
    )]
    NeedsCoordUrl,
    #[error(
        "role agent needs the coordinator's token: on the coordinator run `cat $IDLEMAX_HOME/token`, \
         then pipe it into `idlemax setup --role agent --coord-url <url> --token-stdin` here"
    )]
    NeedsToken,
    #[error(
        "config.toml already sets coordinator.url to {have}, not {want}: edit config.toml to change \
         it, or leave out --coord-url"
    )]
    UrlConflict { have: String, want: String },
}

/// The plan for `req` given what `existing` holds.
pub fn plan(req: &Request<'_>, existing: &Existing<'_>) -> Result<SetupPlan, SetupError> {
    let wanted = req.coord_url.map(normalize_url).transpose()?;
    let (config, coord_url) = match existing.config {
        Some(cfg) => {
            let have = cfg.coordinator.url.trim_end_matches('/');
            if let Some(want) = wanted.as_deref().filter(|w| *w != have) {
                return Err(SetupError::UrlConflict {
                    have: have.to_string(),
                    want: want.to_string(),
                });
            }
            (ConfigPlan::Keep, have.to_string())
        }
        None => {
            if req.role == Role::Agent && wanted.is_none() {
                return Err(SetupError::NeedsCoordUrl);
            }
            let cfg = config_for(req.role, req.os, wanted.as_deref());
            let url = cfg.coordinator.url.clone();
            (ConfigPlan::Write(Box::new(cfg)), url)
        }
    };
    // An agent that reports to itself has no coordinator to report to.
    if req.role == Role::Agent && is_loopback(&coord_url) {
        return Err(SetupError::NeedsCoordUrl);
    }
    let token = match (existing.token, req.token_given) {
        (true, _) => TokenPlan::Keep,
        (false, true) => TokenPlan::FromStdin,
        (false, false) if req.role == Role::Agent => return Err(SetupError::NeedsToken),
        (false, false) => TokenPlan::Generate,
    };
    let (services, service_skipped) = match (req.service, Platform::for_os(req.os)) {
        (false, _) => (Vec::new(), Some("--no-service was given")),
        (true, None) => (
            Vec::new(),
            Some("services are supported on macOS and Linux only"),
        ),
        (true, Some(_)) => (req.role.services().to_vec(), None),
    };
    let statusline = match (req.statusline, req.role.runs_agent()) {
        (false, _) => StatuslinePlan::Skip("not asked for: add --statusline"),
        (true, false) => StatuslinePlan::Skip("this role does not run Claude Code"),
        (true, true) => StatuslinePlan::Install,
    };
    Ok(SetupPlan {
        role: req.role,
        config,
        token,
        coord_url,
        services,
        service_skipped,
        statusline,
    })
}

/// The `config.toml` a new setup starts from: the defaults, this OS's guard profile, and the
/// coordinator address. A coordinator that others must reach listens on that address too.
pub fn config_for(role: Role, os: Os, coord_url: Option<&str>) -> Config {
    let mut cfg = Config::default();
    cfg.agent.profile = Some(DeviceProfile::default_for(os));
    if let Some(url) = coord_url {
        cfg.coordinator.url = url.to_string();
        if let Some(authority) = authority_with_port(url).filter(|_| role.runs_coordinator()) {
            cfg.coordinator.listen = authority.to_string();
        }
    }
    cfg
}

/// What `service install` would do to a unit file that may already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitChange {
    Create,
    Update,
    /// The file already says exactly this, so there is nothing to write or reload.
    Same,
}

pub fn unit_change(existing: Option<&str>, rendered: &str) -> UnitChange {
    match existing {
        None => UnitChange::Create,
        Some(text) if text == rendered => UnitChange::Same,
        Some(_) => UnitChange::Update,
    }
}

/// `url` without trailing slashes, when it is an http or https address with a host.
fn normalize_url(url: &str) -> Result<String, SetupError> {
    let url = url.trim().trim_end_matches('/');
    match authority(url) {
        Some(a) if !a.is_empty() && !a.starts_with(':') => Ok(url.to_string()),
        _ => Err(SetupError::BadUrl(url.to_string())),
    }
}

/// The `host[:port]` part of an http(s) URL.
fn authority(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    rest.split(['/', '?', '#']).next()
}

fn authority_with_port(url: &str) -> Option<&str> {
    let a = authority(url)?;
    let after_host = a.rsplit_once(']').map_or(a, |(_, port)| port);
    after_host.contains(':').then_some(a)
}

/// True for localhost, 127.x.x.x and ::1, which only this device can reach.
pub fn is_loopback(url: &str) -> bool {
    let Some(a) = authority(url) else {
        return false;
    };
    let host = match a.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => a.split(':').next().unwrap_or(a),
    };
    host.eq_ignore_ascii_case("localhost") || host.starts_with("127.") || host == "::1"
}

#[cfg(test)]
mod tests;
