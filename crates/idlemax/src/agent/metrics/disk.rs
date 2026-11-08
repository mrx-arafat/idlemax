//! Adapter: free and total bytes of the filesystems the agent and its jobs write to, from
//! `df -kP <paths>` (no unsafe statfs). Only the named paths are asked about: listing every
//! mount, as `sysinfo::Disks` does, stats a hung network share too and never returns.

use std::path::{Path, PathBuf};

/// (available, total) bytes of the line with the least available space in POSIX `df -kP`
/// output. Parses from the right so filesystem names with spaces still work.
pub fn parse_df_kp(out: &str) -> Option<(u64, u64)> {
    out.lines()
        .skip(1)
        .filter_map(parse_df_line)
        .min_by_key(|(avail, _)| *avail)
}

fn parse_df_line(line: &str) -> Option<(u64, u64)> {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let cap = toks.iter().rposition(|t| t.ends_with('%'))?;
    let kib = |i: usize| toks.get(i)?.parse::<u64>().ok();
    let avail = kib(cap.checked_sub(1)?)?;
    let total = kib(cap.checked_sub(3)?)?;
    Some((avail.saturating_mul(1024), total.saturating_mul(1024)))
}

/// Where the agent and its jobs write: the root (temp files), the state dir (logs, inbox,
/// database) and each running job's working directory, which may sit on another filesystem.
pub fn watched(state_dir: &Path, job_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/"), state_dir.to_path_buf()];
    paths.extend(job_dirs.iter().cloned());
    paths.retain(|p| p.is_absolute());
    paths.sort();
    paths.dedup();
    paths
}

/// (available, total) bytes of the watched filesystem with the least free space. None when
/// `df` cannot run or does not answer within the probe timeout.
#[cfg(unix)]
pub fn read_lowest(paths: &[PathBuf]) -> Option<(u64, u64)> {
    use super::cmd::{run, PROBE_TIMEOUT};
    let mut args = vec!["-kP"];
    args.extend(paths.iter().filter_map(|p| p.to_str()));
    run("/bin/df", &args, PROBE_TIMEOUT)
        .as_deref()
        .and_then(parse_df_kp)
}

// SHORTCUT: Windows measures the disk with the largest total and ignores `paths`; pick the
// drives holding them when a Windows device joins.
#[cfg(not(unix))]
pub fn read_lowest(_paths: &[PathBuf]) -> Option<(u64, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list().ok()?;
    disks
        .list()
        .iter()
        .max_by_key(|d| d.total_space())
        .map(|d| (d.available_space(), d.total_space()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Filesystem 1024-blocks      Used Available Capacity Mounted on\n";

    #[test]
    fn parses_df_sample() {
        let sample = format!("{HEADER}/dev/disk3s1s1 239362496 15000000 123456780      11% /");
        let (avail, total) = parse_df_kp(&sample).expect("parse failed");
        assert_eq!(total, 239362496 * 1024);
        assert_eq!(avail, 123456780 * 1024);
    }

    #[test]
    fn the_filesystem_with_the_least_free_space_wins() {
        let sample = format!(
            "{HEADER}/dev/sda1 100000000 50000000 50000000 50% /\n\
             /dev/sdb1 20000000 19000000 1000000 95% /var/lib/idlemax\n"
        );
        assert_eq!(
            parse_df_kp(&sample),
            Some((1_000_000 * 1024, 20_000_000 * 1024))
        );
    }

    #[test]
    fn watches_the_root_the_state_dir_and_job_dirs_once_and_skips_relative_paths() {
        let jobs = [
            PathBuf::from("/srv/build"),
            PathBuf::from("/srv/build"),
            PathBuf::from("relative"),
        ];
        assert_eq!(
            watched(Path::new("/home/me/.idlemax"), &jobs),
            ["/", "/home/me/.idlemax", "/srv/build"].map(PathBuf::from)
        );
    }

    #[cfg(unix)]
    #[test]
    fn reads_the_root_and_the_temp_dir() {
        let paths = watched(&std::env::temp_dir(), &[]);
        let (avail, total) = read_lowest(&paths).expect("disk space readable");
        assert!(total > 0);
        assert!(avail <= total);
    }
}
