//! Domain: the guard reports its verdicts as text ("pause: on battery"); this maps each known
//! text to the setting or action that changes it. The guard's own output is pinned against
//! this table by a test, so a reworded guard reason fails loudly.

use super::{Reason, ReasonKind};

/// What to do about a reason, and which `[guard]` keys decide it.
enum Fix {
    /// The same advice at every level.
    Text(&'static str),
    /// Advice plus the keys that trip it. A kill-level breach is past the pause line too, so it
    /// holds the job until both keys are relaxed.
    Keys {
        advice: &'static str,
        pause: Option<&'static str>,
        kill: Option<&'static str>,
    },
}

impl Fix {
    fn text(&self, level: Option<&str>) -> String {
        match self {
            Self::Text(text) => (*text).to_string(),
            Self::Keys {
                advice,
                pause,
                kill,
            } => match (level, pause, kill) {
                (Some("kill"), Some(pause), Some(kill)) => format!(
                    "{advice}; past the kill line, so guard.{kill} and guard.{pause} both hold it"
                ),
                (_, Some(key), _) | (_, None, Some(key)) => {
                    format!("{advice}; threshold is guard.{key}")
                }
                (_, None, None) => (*advice).to_string(),
            },
        }
    }
}

const fn keys(
    advice: &'static str,
    pause: Option<&'static str>,
    kill: Option<&'static str>,
) -> Fix {
    Fix::Keys {
        advice,
        pause,
        kill,
    }
}

/// (text the reason starts with, kind, fix). The first match wins, so a longer prefix goes
/// before a shorter one it overlaps.
const TABLE: &[(&str, ReasonKind, Fix)] = &[
    (
        "on battery",
        ReasonKind::Guard,
        Fix::Text("plug in, or run `idlemax config set guard.require_ac_power false`"),
    ),
    (
        "power source unknown",
        ReasonKind::Guard,
        Fix::Text("the agent cannot read the power source; run `idlemax config set guard.require_ac_power false` if this device is always on mains"),
    ),
    (
        "battery",
        ReasonKind::Guard,
        keys("charge it", Some("min_battery_pct"), None),
    ),
    (
        "thermal limited",
        ReasonKind::Guard,
        Fix::Text("let the device cool down, or run `idlemax config set guard.pause_on_thermal false`"),
    ),
    (
        "user active",
        ReasonKind::Guard,
        keys(
            "wait until the device has been idle longer than guard.user_idle_min_secs",
            None,
            None,
        ),
    ),
    (
        "user idle time unknown",
        ReasonKind::Guard,
        Fix::Text("this device cannot report idle time; run `idlemax config set guard.user_idle_min_secs 0` to start jobs without checking for the user"),
    ),
    (
        "kill switch file PAUSE present",
        ReasonKind::LocalPause,
        Fix::Text("delete the PAUSE file in that device's IDLEMAX_HOME"),
    ),
    (
        "local pause active",
        ReasonKind::LocalPause,
        Fix::Text("run `idlemax local resume` on that device"),
    ),
    (
        "memory pressure",
        ReasonKind::Guard,
        keys(
            "wait for memory pressure to ease",
            Some("memory_pressure_pause"),
            Some("memory_pressure_kill"),
        ),
    ),
    (
        "memory free",
        ReasonKind::Guard,
        keys(
            "wait for memory to recover",
            Some("mem_free_pct_pause"),
            Some("mem_free_pct_kill"),
        ),
    ),
    (
        "memory metrics unavailable",
        ReasonKind::Guard,
        Fix::Text("the agent cannot read this device's memory metrics; its log says why"),
    ),
    (
        "MemAvailable",
        ReasonKind::Guard,
        keys(
            "wait for memory to recover",
            Some("mem_available_mib_pause"),
            Some("mem_available_mib_kill"),
        ),
    ),
    (
        "swap-in",
        ReasonKind::Guard,
        keys(
            "wait for swap-in to settle; it must stay high for guard.swap_in_sustain_secs to trip",
            Some("swap_in_pages_per_sec_pause"),
            None,
        ),
    ),
    (
        "swap used",
        ReasonKind::Guard,
        keys(
            "wait for swap to settle",
            Some("swap_used_mib_pause"),
            Some("swap_used_mib_kill"),
        ),
    ),
    (
        "swap",
        ReasonKind::Guard,
        keys(
            "wait for swap to settle",
            Some("swap_growth_pause"),
            Some("swap_growth_kill"),
        ),
    ),
    (
        "PSI memory some",
        ReasonKind::Guard,
        keys("wait for pressure to ease", Some("psi_mem_some_pause"), None),
    ),
    (
        "PSI memory full",
        ReasonKind::Guard,
        keys("wait for pressure to ease", None, Some("psi_mem_full_kill")),
    ),
    (
        "PSI io full",
        ReasonKind::Guard,
        keys(
            "wait for pressure to ease",
            Some("psi_io_full_pause"),
            Some("psi_io_full_kill"),
        ),
    ),
    (
        "load per CPU",
        ReasonKind::Guard,
        keys(
            "wait for the load outside the jobs to drop",
            Some("load_per_cpu_pause"),
            Some("load_per_cpu_kill"),
        ),
    ),
    (
        "load 1m",
        ReasonKind::Guard,
        keys(
            "wait for the load to drop",
            Some("load_1m_pause"),
            Some("load_1m_kill"),
        ),
    ),
    (
        "disk metrics unavailable",
        ReasonKind::Guard,
        Fix::Text("the agent cannot read this device's disk usage; its log says why"),
    ),
    (
        "disk",
        ReasonKind::Guard,
        keys(
            "free disk space",
            Some("disk_free_mib_pause"),
            Some("disk_free_mib_kill"),
        ),
    ),
    (
        "kill-level breach",
        ReasonKind::Guard,
        Fix::Text("nothing to do: the guard pauses first and kills only after guard.kill_after_breaches breaches in a row"),
    ),
    (
        "recovering",
        ReasonKind::Guard,
        Fix::Text("nothing to do: the guard resumes after guard.resume_after_clear clear samples in a row"),
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
        Some((.., fix)) => reason.fix(fix.text(level)),
        None => reason,
    }
}

/// A device marked locally paused whose guard verdict does not say so.
pub(super) fn local_pause() -> Reason {
    Reason::new(ReasonKind::LocalPause, "the device is paused locally")
        .fix("run `idlemax local resume` on that device")
}
