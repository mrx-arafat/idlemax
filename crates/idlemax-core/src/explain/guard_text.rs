//! Domain: the guard reports its verdicts as text ("pause: on battery"); this maps each known
//! text to the setting or action that changes it. The guard's own output is pinned against
//! this table by a test, so a reworded guard reason fails loudly.

use super::{Reason, ReasonKind};

/// (text the reason starts with, kind, fix).
const TABLE: &[(&str, ReasonKind, &str)] = &[
    ("on battery", ReasonKind::Guard, "plug in, or set guard.require_ac_power = false"),
    (
        "power source unknown",
        ReasonKind::Guard,
        "the agent cannot read the power source; set guard.require_ac_power = false if this device is always on mains",
    ),
    ("battery", ReasonKind::Guard, "charge it, or lower guard.min_battery_pct"),
    (
        "thermal limited",
        ReasonKind::Guard,
        "let the device cool down, or set guard.pause_on_thermal = false",
    ),
    (
        "user active",
        ReasonKind::Guard,
        "wait until the device has been idle longer than guard.user_idle_min_secs",
    ),
    (
        "user idle time unknown",
        ReasonKind::Guard,
        "the agent cannot read the idle time on this device; check the agent log",
    ),
    (
        "kill switch file PAUSE present",
        ReasonKind::LocalPause,
        "delete the PAUSE file in that device's IDLEMAX_HOME",
    ),
    (
        "local pause active",
        ReasonKind::LocalPause,
        "run `idlemax local resume` on that device",
    ),
    (
        "memory",
        ReasonKind::Guard,
        "wait for memory to recover; thresholds are guard.mem_free_pct_pause and guard.mem_available_mib_pause",
    ),
    (
        "MemAvailable",
        ReasonKind::Guard,
        "wait for memory to recover; threshold is guard.mem_available_mib_pause",
    ),
    (
        "swap",
        ReasonKind::Guard,
        "wait for swap to settle; thresholds are guard.swap_used_mib_pause and guard.swap_growth_pause",
    ),
    (
        "PSI",
        ReasonKind::Guard,
        "wait for pressure to ease; thresholds are guard.psi_mem_some_pause and guard.psi_io_full_pause",
    ),
    ("load 1m", ReasonKind::Guard, "wait for the load to drop; threshold is guard.load_1m_pause"),
    (
        "disk",
        ReasonKind::Guard,
        "free disk space, or lower guard.disk_free_mib_pause",
    ),
    (
        "kill-level breach",
        ReasonKind::Guard,
        "nothing to do: the guard pauses first and kills only after guard.kill_after_breaches breaches in a row",
    ),
    (
        "recovering",
        ReasonKind::Guard,
        "nothing to do: the guard resumes after guard.resume_after_clear clear samples in a row",
    ),
];

/// "pause: on battery" becomes the detail "on battery" plus the guard level it came with.
fn split(raw: &str) -> (Option<&str>, &str) {
    for level in ["pause", "hold", "kill"] {
        if let Some(detail) = raw
            .strip_prefix(level)
            .and_then(|rest| rest.strip_prefix(": "))
        {
            return (Some(level), detail);
        }
    }
    (None, raw)
}

pub(super) fn reason(raw: &str) -> Reason {
    let (level, detail) = split(raw);
    let known = TABLE.iter().find(|(prefix, ..)| detail.starts_with(prefix));
    let kind = known.map_or(ReasonKind::Guard, |(_, kind, _)| *kind);
    let message = match level {
        Some(level) => format!("{detail} (guard {level})"),
        None => detail.to_string(),
    };
    let reason = Reason::new(kind, message);
    match known {
        Some((.., fix)) => reason.fix(*fix),
        None => reason,
    }
}

/// A device marked locally paused whose guard verdict does not say so.
pub(super) fn local_pause() -> Reason {
    Reason::new(ReasonKind::LocalPause, "the device is paused locally")
        .fix("run `idlemax local resume` on that device")
}
