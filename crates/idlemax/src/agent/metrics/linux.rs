//! Adapter: Linux probes for /proc/pressure, /proc/vmstat, /sys/class/power_supply,
//! /sys/class/thermal and the logind seat idle hint.
//! Every reader takes a filesystem root so tests can point it at a temp dir.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::fs;
use std::path::Path;

use super::Power;

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// `some`/`full` avg10 from a /proc/pressure/<resource> file.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Psi {
    pub some_avg10: Option<f64>,
    pub full_avg10: Option<f64>,
}

pub fn parse_psi(text: &str) -> Psi {
    let mut psi = Psi::default();
    for line in text.lines() {
        let mut toks = line.split_whitespace();
        let kind = toks.next();
        let avg10 = toks
            .find_map(|t| t.strip_prefix("avg10="))
            .and_then(|v| v.parse().ok());
        match kind {
            Some("some") => psi.some_avg10 = avg10,
            Some("full") => psi.full_avg10 = avg10,
            _ => {}
        }
    }
    psi
}

pub fn read_psi(root: &Path, resource: &str) -> Psi {
    read_trim(&root.join("proc/pressure").join(resource))
        .map(|t| parse_psi(&t))
        .unwrap_or_default()
}

/// Cumulative pages swapped in (`pswpin`) from /proc/vmstat.
pub fn read_pswpin(root: &Path) -> Option<u64> {
    let text = fs::read_to_string(root.join("proc/vmstat")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("pswpin ")?.trim().parse().ok())
}

/// AC state and battery charge from /sys/class/power_supply. Peripheral batteries
/// (scope=Device, e.g. a mouse) are ignored. None/None on a server with no supplies.
pub fn read_power(root: &Path) -> Power {
    let Ok(entries) = fs::read_dir(root.join("sys/class/power_supply")) else {
        return Power::default();
    };
    let (mut mains_seen, mut mains_online) = (false, false);
    let mut battery_pct = None;
    let (mut battery_status_known, mut discharging) = (false, false);
    for entry in entries.flatten() {
        let dir = entry.path();
        match read_trim(&dir.join("type")).as_deref() {
            Some("Mains") | Some("USB") => {
                mains_seen = true;
                mains_online |= read_trim(&dir.join("online")).as_deref() == Some("1");
            }
            Some("Battery") if read_trim(&dir.join("scope")).as_deref() != Some("Device") => {
                if battery_pct.is_none() {
                    battery_pct =
                        read_trim(&dir.join("capacity")).and_then(|c| c.parse::<f64>().ok());
                }
                let status = read_trim(&dir.join("status"));
                battery_status_known |= matches!(
                    status.as_deref(),
                    Some("Charging" | "Discharging" | "Not charging" | "Full")
                );
                discharging |= status.as_deref() == Some("Discharging");
            }
            _ => {}
        }
    }
    let on_ac = if mains_seen {
        Some(mains_online)
    } else if battery_pct.is_some() && battery_status_known {
        Some(!discharging)
    } else {
        None
    };
    Power { on_ac, battery_pct }
}

/// True when any thermal zone is at or above one of its passive/hot/critical trip
/// points (passive is where the kernel starts throttling). None with no zones (VMs).
pub fn read_thermal_limited(root: &Path) -> Option<bool> {
    let entries = fs::read_dir(root.join("sys/class/thermal")).ok()?;
    let mut any = false;
    let mut limited = false;
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("thermal_zone")
        {
            continue;
        }
        let zone = entry.path();
        let Some(temp) = read_trim(&zone.join("temp")).and_then(|t| t.parse::<i64>().ok()) else {
            continue;
        };
        any = true;
        for i in 0.. {
            let Some(kind) = read_trim(&zone.join(format!("trip_point_{i}_type"))) else {
                break;
            };
            if !matches!(kind.as_str(), "passive" | "hot" | "critical") {
                continue;
            }
            let trip = read_trim(&zone.join(format!("trip_point_{i}_temp")))
                .and_then(|t| t.parse::<i64>().ok());
            // A trip of 0 or less means disabled on many drivers.
            if trip.is_some_and(|t| t > 0 && temp >= t) {
                limited = true;
            }
        }
    }
    any.then_some(limited)
}

/// Idle seconds from `loginctl show-seat seat0 -p CanGraphical -p IdleHint -p IdleSinceHint`.
/// Only graphical seats count; headless servers give None. IdleHint=no means the
/// session is active, reported as 0 so the guard stays conservative.
pub fn parse_logind_seat(out: &str, now_us: u64) -> Option<u64> {
    let get = |key: &str| {
        out.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim())
        })
    };
    if get("CanGraphical") != Some("yes") {
        return None;
    }
    match get("IdleHint")? {
        "yes" => {
            let since: u64 = get("IdleSinceHint")?.parse().ok()?;
            (since > 0).then(|| now_us.saturating_sub(since) / 1_000_000)
        }
        _ => Some(0),
    }
}

/// Runs loginctl only when logind has a seat0 (desktops); headless servers skip it.
pub fn run_logind_seat(root: &Path) -> Option<String> {
    use super::cmd::{run, PROBE_TIMEOUT};
    if !root.join("run/systemd/seats/seat0").exists() {
        return None;
    }
    let props = [
        "-p",
        "CanGraphical",
        "-p",
        "IdleHint",
        "-p",
        "IdleSinceHint",
    ];
    let mut args = vec!["show-seat", "seat0"];
    args.extend(props);
    run("loginctl", &args, PROBE_TIMEOUT)
}

/// Raw Linux readings; mapped into MetricsSnapshot by the sampler.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LinuxReadings {
    pub user_idle_secs: Option<u64>,
    pub power: Power,
    pub thermal_limited: Option<bool>,
    pub psi_mem: Psi,
    pub psi_io: Psi,
    pub pswpin: Option<u64>,
}

pub fn read(root: &Path, logind_out: Option<&str>, now_us: u64) -> LinuxReadings {
    LinuxReadings {
        user_idle_secs: logind_out.and_then(|o| parse_logind_seat(o, now_us)),
        power: read_power(root),
        thermal_limited: read_thermal_limited(root),
        psi_mem: read_psi(root, "memory"),
        psi_io: read_psi(root, "io"),
        pswpin: read_pswpin(root),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn power_ignores_peripherals_and_unknown_battery_status() {
        let tmp = tempfile::tempdir().unwrap();
        let battery = "sys/class/power_supply/BAT0";
        write(tmp.path(), &format!("{battery}/type"), "Battery\n");
        write(tmp.path(), &format!("{battery}/capacity"), "41\n");
        write(tmp.path(), &format!("{battery}/status"), "Unknown\n");
        assert_eq!(
            read_power(tmp.path()),
            Power {
                on_ac: None,
                battery_pct: Some(41.0),
            }
        );

        write(tmp.path(), &format!("{battery}/status"), "Discharging\n");
        write(
            tmp.path(),
            "sys/class/power_supply/hidpp_battery_0/type",
            "Battery\n",
        );
        write(
            tmp.path(),
            "sys/class/power_supply/hidpp_battery_0/scope",
            "Device\n",
        );
        write(
            tmp.path(),
            "sys/class/power_supply/hidpp_battery_0/capacity",
            "99\n",
        );
        assert_eq!(read_power(tmp.path()).on_ac, Some(false));

        write(tmp.path(), "sys/class/power_supply/AC/type", "Mains\n");
        write(tmp.path(), "sys/class/power_supply/AC/online", "1\n");
        assert_eq!(read_power(tmp.path()).on_ac, Some(true));
    }
}
