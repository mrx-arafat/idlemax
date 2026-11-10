//! Adapter: read effective Linux cgroup v1/v2 memory limits, including ancestors.

use idlemax_core::config::MemoryCap;
#[cfg(any(target_os = "linux", test))]
use std::path::{Component, Path};

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
    let Ok(membership) = std::fs::read_to_string(proc_cgroup) else {
        return MemoryCap::Unknown;
    };
    let mut unified = None;
    for line in membership.lines() {
        let parts: Vec<_> = line.splitn(3, ':').collect();
        if parts.len() != 3 {
            continue;
        }
        if parts[1].split(',').any(|controller| controller == "memory") {
            return hierarchy_limit(
                &mount.join("memory"),
                parts[2],
                "memory.limit_in_bytes",
                true,
            );
        }
        if parts[0] == "0" && parts[1].is_empty() {
            unified = Some(parts[2]);
        }
    }
    unified.map_or(MemoryCap::Unknown, |path| {
        hierarchy_limit(mount, path, "memory.max", false)
    })
}

#[cfg(any(target_os = "linux", test))]
fn hierarchy_limit(root: &Path, membership: &str, file: &str, v1: bool) -> MemoryCap {
    let Ok(relative) = Path::new(membership).strip_prefix("/") else {
        return MemoryCap::Unknown;
    };
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return MemoryCap::Unknown;
    }
    let mut directory = root.join(relative);
    let mut known = false;
    let mut limit = None;
    loop {
        match std::fs::read_to_string(directory.join(file)) {
            Ok(text) => {
                known = true;
                let text = text.trim();
                if text != "max" && !(v1 && text == "-1") {
                    let Ok(bytes) = text.parse::<u64>() else {
                        return MemoryCap::Unknown;
                    };
                    // Linux v1 reports a page-rounded LONG_MAX for unlimited. Values
                    // above 1 EiB include that sentinel, far beyond useful host capacity.
                    if !v1 || bytes < (1 << 60) {
                        limit = Some(limit.map_or(bytes, |old: u64| old.min(bytes)));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return MemoryCap::Unknown,
        }
        if directory == root {
            break;
        }
        if !directory.pop() {
            return MemoryCap::Unknown;
        }
    }
    match limit {
        Some(bytes) => MemoryCap::Limited(bytes),
        None if known => MemoryCap::Unlimited,
        None => MemoryCap::Unknown,
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
}
