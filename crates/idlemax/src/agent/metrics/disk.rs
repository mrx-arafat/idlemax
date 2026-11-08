//! Adapter: free and total bytes of the root filesystem using sysinfo (no subprocess spawn).

use std::path::Path;

/// (available, total) bytes from POSIX `df -kP` output. Parses from the right so
/// filesystem names with spaces still work. Kept for POSIX df fixture testing.
#[allow(dead_code)]
pub fn parse_df_kp(out: &str) -> Option<(u64, u64)> {
    let line = out.lines().skip(1).find(|l| !l.trim().is_empty())?;
    let toks: Vec<&str> = line.split_whitespace().collect();
    let cap = toks.iter().rposition(|t| t.ends_with('%'))?;
    let kib = |i: usize| toks.get(i)?.parse::<u64>().ok();
    let avail = kib(cap.checked_sub(1)?)?;
    let total = kib(cap.checked_sub(3)?)?;
    Some((avail.saturating_mul(1024), total.saturating_mul(1024)))
}

/// Reads free and total bytes of the root filesystem without spawning a subprocess.
pub fn read_root() -> Option<(u64, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    let root = disks
        .iter()
        .find(|d| d.mount_point() == Path::new("/"))
        .or_else(|| disks.iter().max_by_key(|d| d.total_space()))?;
    Some((root.available_space(), root.total_space()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_df_sample() {
        let sample = "Filesystem 1024-blocks      Used Available Capacity Mounted on\n\
                      /dev/disk3s1s1 239362496 15000000 123456780      11% /";
        let (avail, total) = parse_df_kp(sample).expect("parse failed");
        assert_eq!(total, 239362496 * 1024);
        assert_eq!(avail, 123456780 * 1024);
    }

    #[test]
    fn read_root_returns_positive_space() {
        let (avail, total) = read_root().expect("root disk space readable");
        assert!(total > 0);
        assert!(avail <= total);
    }
}
