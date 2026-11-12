//! Adapter: read effective Linux cgroup v1/v2 memory limits, including ancestors, and what
//! the cgroup holding the tightest one uses.

use idlemax_core::config::MemoryCap;
#[cfg(any(target_os = "linux", test))]
use std::path::{Component, Path, PathBuf};

pub(super) fn memory_max() -> MemoryCap {
    #[cfg(target_os = "linux")]
    {
        read_memory_max(Path::new("/proc/self/cgroup"), Path::new("/sys/fs/cgroup"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        MemoryCap::Unlimited
    }
}

#[cfg(any(target_os = "linux", test))]
fn read_memory_max(proc_cgroup: &Path, mount: &Path) -> MemoryCap {
    read_limit(proc_cgroup, mount).0
}

/// What the cgroup with the tightest limit uses, page cache left out because the kernel
/// gives it back before it kills: `memory.current` less `inactive_file` (v2), or
/// `memory.usage_in_bytes` less `total_inactive_file` (v1). `None` without a limit or a reading.
// SHORTCUT: unused until workers run inside the agent's cgroup, wire it with the headless host
// (Spec 5 Task 8); pinned only by memory_used_reads_the_cgroup_with_the_tightest_limit_without_page_cache.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(test), allow(dead_code))]
fn read_memory_used(proc_cgroup: &Path, mount: &Path) -> Option<u64> {
    let (MemoryCap::Limited(_), Some((directory, v1))) = read_limit(proc_cgroup, mount) else {
        return None;
    };
    let (current, inactive) = if v1 {
        ("memory.usage_in_bytes", "total_inactive_file")
    } else {
        ("memory.current", "inactive_file")
    };
    let used: u64 = std::fs::read_to_string(directory.join(current))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let stat = std::fs::read_to_string(directory.join("memory.stat")).ok()?;
    let cache: u64 = stat.lines().find_map(|line| {
        let (key, value) = line.split_once(' ')?;
        (key == inactive).then(|| value.trim().parse().ok())?
    })?;
    Some(used.saturating_sub(cache))
}

/// The effective limit, and the directory that holds it with whether it is cgroup v1.
#[cfg(any(target_os = "linux", test))]
fn read_limit(proc_cgroup: &Path, mount: &Path) -> (MemoryCap, Option<(PathBuf, bool)>) {
    let Ok(membership) = std::fs::read_to_string(proc_cgroup) else {
        return (MemoryCap::Unknown, None);
    };
    let mut unified = None;
    for line in membership.lines() {
        let parts: Vec<_> = line.splitn(3, ':').collect();
        if parts.len() != 3 {
            continue;
        }
        if parts[1].split(',').any(|controller| controller == "memory") {
            let (cap, directory) = hierarchy_limit(
                &mount.join("memory"),
                parts[2],
                "memory.limit_in_bytes",
                true,
            );
            return (cap, directory.map(|d| (d, true)));
        }
        if parts[0] == "0" && parts[1].is_empty() {
            unified = Some(parts[2]);
        }
    }
    unified.map_or((MemoryCap::Unknown, None), |path| {
        let (cap, directory) = hierarchy_limit(mount, path, "memory.max", false);
        (cap, directory.map(|d| (d, false)))
    })
}

#[cfg(any(target_os = "linux", test))]
fn hierarchy_limit(
    root: &Path,
    membership: &str,
    file: &str,
    v1: bool,
) -> (MemoryCap, Option<PathBuf>) {
    const UNKNOWN: (MemoryCap, Option<PathBuf>) = (MemoryCap::Unknown, None);
    let Ok(relative) = Path::new(membership).strip_prefix("/") else {
        return UNKNOWN;
    };
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return UNKNOWN;
    }
    let mut directory = root.join(relative);
    let mut known = false;
    let mut limit: Option<(u64, PathBuf)> = None;
    loop {
        match std::fs::read_to_string(directory.join(file)) {
            Ok(text) => {
                known = true;
                let text = text.trim();
                if text != "max" && !(v1 && text == "-1") {
                    let Ok(bytes) = text.parse::<u64>() else {
                        return UNKNOWN;
                    };
                    // Linux v1 reports a page-rounded LONG_MAX for unlimited. Values
                    // above 1 EiB include that sentinel, far beyond useful host capacity.
                    if (!v1 || bytes < (1 << 60))
                        && limit.as_ref().is_none_or(|(old, _)| bytes < *old)
                    {
                        limit = Some((bytes, directory.clone()));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return UNKNOWN,
        }
        if directory == root {
            break;
        }
        if !directory.pop() {
            return UNKNOWN;
        }
    }
    match limit {
        Some((bytes, directory)) => (MemoryCap::Limited(bytes), Some(directory)),
        None if known => (MemoryCap::Unlimited, None),
        None => UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effective_memory_limits_cover_v2_v1_hybrid_parents_and_unknown() {
        for (membership, v1) in [
            ("0::/slice/agent\n", false),
            ("4:cpu,memory:/slice/agent\n", true),
            ("0::/unified\n4:memory:/slice/agent\n", true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let proc = temp.path().join("cgroup");
            std::fs::write(&proc, membership).unwrap();
            let root = if v1 {
                temp.path().join("memory")
            } else {
                temp.path().to_path_buf()
            };
            let file = if v1 {
                "memory.limit_in_bytes"
            } else {
                "memory.max"
            };
            std::fs::create_dir_all(root.join("slice/agent")).unwrap();
            let unlimited = if v1 { "9223372036854771712" } else { "max" };
            std::fs::write(root.join(file), unlimited).unwrap();
            std::fs::write(root.join("slice").join(file), "536870912").unwrap();
            std::fs::write(root.join("slice/agent").join(file), "671088640").unwrap();
            assert_eq!(
                read_memory_max(&proc, temp.path()),
                MemoryCap::Limited(536870912)
            );
            std::fs::write(root.join("slice").join(file), unlimited).unwrap();
            assert_eq!(
                read_memory_max(&proc, temp.path()),
                MemoryCap::Limited(671088640)
            );
            std::fs::write(root.join("slice/agent").join(file), unlimited).unwrap();
            assert_eq!(read_memory_max(&proc, temp.path()), MemoryCap::Unlimited);
            std::fs::write(root.join("slice/agent").join(file), "invalid").unwrap();
            assert_eq!(read_memory_max(&proc, temp.path()), MemoryCap::Unknown);
            std::fs::remove_dir_all(root.join("slice")).unwrap();
            std::fs::remove_file(root.join(file)).unwrap();
            assert_eq!(read_memory_max(&proc, temp.path()), MemoryCap::Unknown);
            std::fs::create_dir(root.join(file)).unwrap();
            assert_eq!(read_memory_max(&proc, temp.path()), MemoryCap::Unknown);
        }
    }

    #[test]
    fn memory_used_reads_the_cgroup_with_the_tightest_limit_without_page_cache() {
        for (membership, v1) in [
            ("0::/slice/agent\n", false),
            ("4:memory:/slice/agent\n", true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let proc = temp.path().join("cgroup");
            std::fs::write(&proc, membership).unwrap();
            let root = if v1 {
                temp.path().join("memory")
            } else {
                temp.path().to_path_buf()
            };
            let (limit, current, stat) = if v1 {
                (
                    "memory.limit_in_bytes",
                    "memory.usage_in_bytes",
                    "total_inactive_file",
                )
            } else {
                ("memory.max", "memory.current", "inactive_file")
            };
            for (dir, cap, used, cache) in [
                ("slice", "1000", "900", "100"),
                ("slice/agent", "2000", "500", "50"),
            ] {
                let dir = root.join(dir);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(limit), cap).unwrap();
                std::fs::write(dir.join(current), used).unwrap();
                let text = format!("active_file 7\n{stat} {cache}\n");
                std::fs::write(dir.join("memory.stat"), text).unwrap();
            }
            assert_eq!(read_memory_used(&proc, temp.path()), Some(800), "v1 {v1}");
            std::fs::remove_file(root.join("slice").join(current)).unwrap();
            assert_eq!(read_memory_used(&proc, temp.path()), None, "v1 {v1}");
        }
    }
}
