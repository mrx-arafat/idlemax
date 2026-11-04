//! Adapter: free and total bytes of the root filesystem (`df -kP /`, no unsafe statfs).

/// (available, total) bytes from POSIX `df -kP` output. Parses from the right so
/// filesystem names with spaces still work.
pub fn parse_df_kp(out: &str) -> Option<(u64, u64)> {
    let line = out.lines().skip(1).find(|l| !l.trim().is_empty())?;
    let toks: Vec<&str> = line.split_whitespace().collect();
    let cap = toks.iter().rposition(|t| t.ends_with('%'))?;
    let kib = |i: usize| toks.get(i)?.parse::<u64>().ok();
    let avail = kib(cap.checked_sub(1)?)?;
    let total = kib(cap.checked_sub(3)?)?;
    Some((avail.saturating_mul(1024), total.saturating_mul(1024)))
}

#[cfg(unix)]
pub fn read_root() -> Option<(u64, u64)> {
    use super::cmd::{run, PROBE_TIMEOUT};
    run("/bin/df", &["-kP", "/"], PROBE_TIMEOUT)
        .as_deref()
        .and_then(parse_df_kp)
}

// SHORTCUT: Windows measures the disk with the largest total; pick the system drive
// when a Windows device joins.
#[cfg(not(unix))]
pub fn read_root() -> Option<(u64, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks
        .list()
        .iter()
        .max_by_key(|d| d.total_space())
        .map(|d| (d.available_space(), d.total_space()))
}
