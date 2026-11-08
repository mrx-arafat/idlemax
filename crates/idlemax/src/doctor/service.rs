//! Adapter: reads the installed service unit, if any. Never runs launchctl or systemctl.

use std::path::{Path, PathBuf};

use idlemax_core::doctor::{Check, Report};
use idlemax_core::install::service::Platform;
use idlemax_core::install::unit_read::read_unit;

use super::facts;

const PLACEHOLDERS: [&str; 3] = ["@BIN@", "@HOME@", "@PATH@"];

/// `kind` is "agent" or "coordinator"; a coordinator-only machine has the latter.
fn candidates(home: Option<&Path>) -> Vec<(&'static str, PathBuf)> {
    let mut units: Vec<(&'static str, PathBuf)> = Vec::new();
    if cfg!(target_os = "macos") {
        for label in ["com.idlemax.agent.plist", "com.idlemax.coord.plist"] {
            let kind = if label.contains("agent") {
                "agent"
            } else {
                "coordinator"
            };
            if let Some(home) = home {
                units.push((kind, home.join("Library/LaunchAgents").join(label)));
            }
            units.push((kind, Path::new("/Library/LaunchDaemons").join(label)));
        }
    } else if cfg!(target_os = "linux") {
        for name in ["idlemax-agent.service", "idlemax-coord.service"] {
            let kind = if name.contains("agent") {
                "agent"
            } else {
                "coordinator"
            };
            units.push((kind, Path::new("/etc/systemd/system").join(name)));
            if let Some(home) = home {
                units.push((kind, home.join(".config/systemd/user").join(name)));
            }
        }
    }
    units
}

/// An idlemax service unit found on disk.
pub struct Found {
    /// "agent" or "coordinator".
    pub kind: &'static str,
    pub path: PathBuf,
    pub text: String,
}

/// Every installed unit, the agent's first.
pub fn installed(home: Option<&Path>) -> Vec<Found> {
    candidates(home)
        .into_iter()
        .filter_map(|(kind, path)| facts::read_text(&path).map(|text| Found { kind, path, text }))
        .collect()
}

pub fn agent_installed(units: &[Found]) -> bool {
    units.iter().any(|u| u.kind == "agent")
}

/// The PATH the agent service starts with, from its unit.
pub fn agent_path(units: &[Found]) -> Option<String> {
    let agent = units.iter().find(|u| u.kind == "agent")?;
    read_unit(platform(), &agent.text).path
}

fn platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::Launchd
    } else {
        Platform::SystemdUser
    }
}

pub fn check(report: &mut Report, units: &[Found]) {
    let Some(Found { kind, path, text }) = units.first() else {
        report.push(Check::warn(
            "service",
            "no idlemax service unit installed: the agent stops when its terminal closes",
            install_fix(),
        ));
        return;
    };
    let missing: Vec<&str> = PLACEHOLDERS
        .iter()
        .copied()
        .filter(|p| text.contains(p))
        .collect();
    if !missing.is_empty() {
        report.push(Check::warn(
            "service",
            format!(
                "{} still contains {}",
                facts::short(path),
                missing.join(", ")
            ),
            "this file is an unrendered template: run `idlemax service install`, which fills the placeholders in and reloads it",
        ));
        return;
    }
    let bin = read_unit(platform(), text).bin;
    match bin.as_deref().filter(|b| Path::new(b).is_absolute()) {
        Some(bin) if !facts::is_executable(Path::new(bin)) => report.push(Check::fail(
            "service",
            format!("{kind} unit {} runs {bin}, which is missing or not executable", facts::short(path)),
            format!("put idlemax at {bin}, or run `idlemax service install --bin /full/path/idlemax` to point a per-user service at it"),
        )),
        _ => report.push(Check::ok(
            "service",
            format!("{kind} unit {}", facts::short(path)),
        )),
    }
}

fn install_fix() -> String {
    if cfg!(target_os = "macos") {
        "run `idlemax service install` (add --dry-run to preview the file and commands)".into()
    } else if cfg!(target_os = "linux") {
        "run `idlemax service install` for a per-user service, or install the system units from packaging/linux as the README's \"Deploying across devices\" describes".into()
    } else {
        // SHORTCUT: no Windows service, start the agent at login when a Windows device joins.
        "service install is not supported on this OS yet: start `idlemax agent` at login yourself"
            .into()
    }
}
