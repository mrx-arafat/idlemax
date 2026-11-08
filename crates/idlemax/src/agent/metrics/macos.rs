//! Adapter: macOS probes for ioreg HIDIdleTime, pmset batt/therm and sysctl memorystatus.
//! Parsers are pure and compiled everywhere so their tests run on any host.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use super::Power;

/// Seconds since the last HID event from `ioreg -r -c IOHIDSystem -k HIDIdleTime -d 1`
/// (the value is in nanoseconds).
pub fn parse_hid_idle_secs(out: &str) -> Option<u64> {
    out.lines().find_map(|line| {
        let (key, val) = line.split_once('=')?;
        if key.trim().trim_matches('"') != "HIDIdleTime" {
            return None;
        }
        val.trim().parse::<u64>().ok().map(|ns| ns / 1_000_000_000)
    })
}

/// Power source and charge from `pmset -g batt`. UPS power counts as not on AC.
pub fn parse_pmset_batt(out: &str) -> Power {
    let on_ac = out.lines().find_map(|line| {
        let rest = line.split("drawing from '").nth(1)?;
        let source = rest.split('\'').next()?;
        Some(source == "AC Power")
    });
    let battery_pct = out
        .lines()
        .filter(|l| l.contains("InternalBattery"))
        .find_map(|l| {
            l.split(|c: char| c.is_whitespace() || c == ';')
                .find_map(|tok| tok.strip_suffix('%')?.parse::<f64>().ok())
        });
    Power { on_ac, battery_pct }
}

/// Thermal throttling from `pmset -g therm`: any CPU_Speed_Limit < 100 or a recorded
/// non-zero warning level. None when the output says nothing recognizable.
pub fn parse_pmset_therm(out: &str) -> Option<bool> {
    let mut seen = false;
    let mut limited = false;
    for line in out.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some((key, val)) = line.split_once('=') {
            if key.trim() == "CPU_Speed_Limit" {
                if let Ok(v) = val.trim().parse::<u32>() {
                    seen = true;
                    limited |= v < 100;
                }
                continue;
            }
        }
        if lower.contains("warning level") {
            seen = true;
            if !lower.contains("no ") {
                let level = line
                    .split(|c: char| !c.is_ascii_digit())
                    .rfind(|s| !s.is_empty())
                    .and_then(|s| s.parse::<u32>().ok());
                limited |= level.is_some_and(|n| n > 0);
            }
        }
    }
    seen.then_some(limited)
}

/// `name: value` pairs from `sysctl <names...>`; unknown oids are simply absent.
pub fn parse_sysctl<'a>(out: &'a str, name: &str) -> Option<&'a str> {
    out.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == name).then(|| v.trim())
    })
}

/// Raw macOS readings; mapped into MetricsSnapshot by the sampler.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MacReadings {
    pub user_idle_secs: Option<u64>,
    pub power: Power,
    pub thermal_limited: Option<bool>,
    /// kern.memorystatus_level, the same number `memory_pressure` prints as free %.
    pub mem_free_pct: Option<f64>,
    /// kern.memorystatus_vm_pressure_level: 1 normal, 2 warn, 4 critical.
    pub vm_pressure_level: Option<u32>,
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn read() -> MacReadings {
    read_with_cache(None)
}

#[cfg(target_os = "macos")]
pub fn read_with_cache(cached_therm: Option<Option<bool>>) -> MacReadings {
    use super::cmd::{run, PROBE_TIMEOUT};
    let idle = run(
        "/usr/sbin/ioreg",
        &["-r", "-c", "IOHIDSystem", "-k", "HIDIdleTime", "-d", "1"],
        PROBE_TIMEOUT,
    );
    let batt = run("/usr/bin/pmset", &["-g", "batt"], PROBE_TIMEOUT);
    let therm = match cached_therm {
        Some(cached) => cached,
        None => run("/usr/bin/pmset", &["-g", "therm"], PROBE_TIMEOUT)
            .as_deref()
            .and_then(parse_pmset_therm),
    };
    let sysctl = run(
        "/usr/sbin/sysctl",
        &[
            "kern.memorystatus_level",
            "kern.memorystatus_vm_pressure_level",
        ],
        PROBE_TIMEOUT,
    )
    .unwrap_or_default();
    MacReadings {
        user_idle_secs: idle.as_deref().and_then(parse_hid_idle_secs),
        power: batt.as_deref().map(parse_pmset_batt).unwrap_or_default(),
        thermal_limited: therm,
        mem_free_pct: parse_sysctl(&sysctl, "kern.memorystatus_level")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| (0.0..=100.0).contains(v)),
        vm_pressure_level: parse_sysctl(&sysctl, "kern.memorystatus_vm_pressure_level")
            .and_then(|v| v.parse().ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hid_idle_nanoseconds() {
        let output = "      \"HIDIdleTime\" = 12345678900\n";
        assert_eq!(parse_hid_idle_secs(output), Some(12));
    }

    #[test]
    fn parses_pmset_batt_ac_and_battery() {
        let output = "Now drawing from 'AC Power'\n\
                      -InternalBattery-0 (id=123) 85%; charging; 1:30 remaining present: true\n";
        let power = parse_pmset_batt(output);
        assert_eq!(power.on_ac, Some(true));
        assert_eq!(power.battery_pct, Some(85.0));
    }

    #[test]
    fn parses_pmset_therm_limits() {
        let clean = "Note: No thermal warning level has been recorded\n\
                     Note: No performance warning level has been recorded\n";
        assert_eq!(parse_pmset_therm(clean), Some(false));

        let throttled = "CPU_Speed_Limit = 80\n";
        assert_eq!(parse_pmset_therm(throttled), Some(true));
    }

    #[test]
    fn parses_sysctl_values() {
        let sample = "kern.memorystatus_level: 42\n\
                      kern.memorystatus_vm_pressure_level: 1\n";
        assert_eq!(parse_sysctl(sample, "kern.memorystatus_level"), Some("42"));
        assert_eq!(
            parse_sysctl(sample, "kern.memorystatus_vm_pressure_level"),
            Some("1")
        );
        assert_eq!(parse_sysctl(sample, "nonexistent"), None);
    }
}
