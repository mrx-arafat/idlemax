//! Adapter: finds the process behind a TCP connection on this host (Linux `/proc`, macOS
//! `lsof`) and reads the environment of it and its ancestors with `sysinfo`, and on Linux
//! the cgroup of each, so a worker that left its parents is still known by its scope.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use idlemax_core::control::caller::{in_worker_scope, mark_of, same_host, Link, Mark};
use sysinfo::{Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::Seen;
use crate::coord::server::Conn;

/// The walk stops after this many parents.
const MAX_PARENTS: usize = 64;

/// The OS lookup of `CallerCheck`.
pub fn find(conn: Conn, homes: &[PathBuf]) -> Seen {
    let peer = conn.peer.ip();
    // SHORTCUT: no caller lookup on Windows, add one when a Windows device joins. Until then
    // a caller there is judged as remote, by the token alone.
    let lookup_exists = cfg!(any(target_os = "linux", target_os = "macos"));
    let same_host = lookup_exists && (same_host(peer, &[]) || same_host(peer, &local_ips()));
    let chain = if same_host {
        peer_pid(conn).and_then(|pid| chain(pid, homes))
    } else {
        None
    };
    Seen { same_host, chain }
}

/// This host's interface addresses.
fn local_ips() -> Vec<IpAddr> {
    let Ok(networks) = Networks::new_with_refreshed_list() else {
        return Vec::new();
    };
    networks
        .values()
        .flat_map(|data| data.ip_networks().iter().map(|n| n.addr))
        .collect()
}

/// The address as the OS prints an IPv4 socket, when the server saw it IPv4-mapped.
fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// The pid that owns the client end: the socket's inode in `/proc/net/tcp{,6}`, then the
/// process with a descriptor on that inode.
#[cfg(target_os = "linux")]
fn peer_pid(conn: Conn) -> Option<u32> {
    use idlemax_core::control::caller::socket_inode;
    use std::fs;

    let inode = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .find_map(|file| {
            let text = fs::read_to_string(file).ok()?;
            socket_inode(&text, conn.peer, conn.local)
                .or_else(|| socket_inode(&text, canonical(conn.peer), canonical(conn.local)))
        })?;
    let wanted = format!("socket:[{inode}]");
    fs::read_dir("/proc").ok()?.flatten().find_map(|entry| {
        let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
        let fds = fs::read_dir(entry.path().join("fd")).ok()?;
        fds.flatten()
            .any(|fd| fs::read_link(fd.path()).is_ok_and(|l| l.as_os_str() == wanted.as_str()))
            .then_some(pid)
    })
}

/// The pid that owns the client end, from `lsof`.
#[cfg(target_os = "macos")]
fn peer_pid(conn: Conn) -> Option<u32> {
    use idlemax_core::control::caller::peer_pid_from_lsof;

    let peer = canonical(conn.peer);
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            "-a",
            &format!("-iTCP@{peer}"),
            "-sTCP:ESTABLISHED",
            "-Fpn",
        ])
        .output()
        .ok()?;
    peer_pid_from_lsof(&String::from_utf8_lossy(&out.stdout), peer)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_pid(_conn: Conn) -> Option<u32> {
    None
}

/// `pid` and up to `MAX_PARENTS` ancestors, each with its mark; `None` when `pid` is gone.
/// An environment that cannot be read gives the mark `None` and the walk goes on.
fn chain(pid: u32, homes: &[PathBuf]) -> Option<Vec<Link>> {
    let mut sys = System::new().ok()?;
    let kind = ProcessRefreshKind::nothing()
        .with_environ(UpdateKind::Always)
        .without_tasks();
    let mut links = Vec::new();
    let mut next = Some(Pid::from_u32(pid));
    while let Some(pid) = next {
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), false, kind);
        let Some(process) = sys.process(pid) else {
            break;
        };
        let env: Vec<(String, String)> = process
            .environ()
            .iter()
            .filter_map(|e| e.to_str()?.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let scoped = std::fs::read_to_string(format!("/proc/{}/cgroup", pid.as_u32()))
            .is_ok_and(|text| in_worker_scope(&text));
        let mark = match scoped {
            true => Mark::ThisHome,
            false => mark_of(&env, homes),
        };
        links.push(Link {
            pid: pid.as_u32(),
            mark,
        });
        next = process.parent().filter(|p| *p != pid);
        if links.len() > MAX_PARENTS {
            break;
        }
    }
    (!links.is_empty()).then_some(links)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use idlemax_core::control::caller::Mark;

    use super::chain;
    use crate::agent::worker::scope::WorkerScope;

    #[test]
    fn a_process_in_a_worker_scope_is_marked_without_any_marker_in_its_environment() {
        // Red team case 2: `env -u IDLEMAX_JOB_ID setsid -f idlemax mode auto` from a worker.
        let Some(scope) = WorkerScope::detect() else {
            eprintln!("no systemd manager here: the scope case is not tested");
            return;
        };
        let sleep = crate::agent::runner::wrapper::which("sleep").unwrap();
        let mut child = scope.command(7, 1, &sleep).arg("30").spawn().unwrap();
        let pid = child.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut marked = false;
        while !marked && std::time::Instant::now() < deadline {
            marked = chain(pid, &[]).is_some_and(|c| c[0].mark == Mark::ThisHome);
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(marked, "a scoped process was taken for the owner");
    }
}
