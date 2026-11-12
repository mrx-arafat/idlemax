//! Domain: decide who is calling, the owner or a process this idlemax launched, and find
//! the calling process in OS output (`lsof`, `/proc/net/tcp`). Pure: no I/O, no clock.
//!
//! A launched job or worker carries `LAUNCHED_ENV` in its environment, and on Linux a worker
//! runs in a systemd scope of its own; the adapter walks the caller's process chain, reads
//! each environment and cgroup and asks `classify`. A local caller whose
//! process cannot be found is `Unseen` and fails closed for owner-only calls.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};

use crate::paths::HOME_ENV;
use crate::types::ControlAction;

/// The one marker of a process idlemax launched: the runner sets it on every job.
pub const LAUNCHED_ENV: &str = crate::job_run::JOB_ID_ENV;

/// What a call needs from its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Stop,
    Owner,
}

/// Who sent a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Owner,
    /// A job or worker this idlemax launched; `pid` is the marked process.
    Launched {
        pid: u32,
    },
    /// On this host, but its process was not found.
    Unseen,
}

/// The marker one process carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    ThisHome,
    /// Launched by an idlemax with another state dir.
    OtherHome,
}

/// One process of the caller's chain, the caller first, then its ancestors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub pid: u32,
    pub mark: Mark,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallerError {
    #[error(
        "this call comes from a job or worker that idlemax started (pid {0}); only the owner \
         can do this, from their own terminal, the dashboard or the TUI"
    )]
    Launched(u32),
    /// The CLI or `idlemax mcp` found the marker in its own environment; its own pid would
    /// send a reader looking for the wrong process, so the text names none.
    #[error(
        "this call comes from a job or worker that idlemax started (its environment has \
         IDLEMAX_JOB_ID); only the owner can do this, from their own terminal, the dashboard \
         or the TUI"
    )]
    LaunchedHere,
    #[error(
        "idlemax could not find the program that sent this call, so it is not treated as the \
         owner; stop and pause still work"
    )]
    Unseen,
}

/// The mark of one process from its environment pairs. `homes` is this server's state dir
/// as written and as canonical path. A marked process without `IDLEMAX_HOME` counts as this
/// home, so the rule fails closed.
pub fn mark_of(env: &[(String, String)], homes: &[PathBuf]) -> Mark {
    let value = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    if value(LAUNCHED_ENV).is_none() {
        return Mark::None;
    }
    match value(HOME_ENV) {
        Some(home) if !home.is_empty() && !homes.iter().any(|h| h == Path::new(home)) => {
            Mark::OtherHome
        }
        _ => Mark::ThisHome,
    }
}

/// The name every worker's systemd scope starts with (red team cases 2 and 10).
pub const WORKER_SCOPE_PREFIX: &str = "idlemax-w-";

/// Whether `/proc/<pid>/cgroup` text puts the process in a worker's scope. A process can drop
/// the marker from its environment and leave its parents with `setsid`, but it cannot leave
/// its cgroup, so such a process counts as launched (as this home: a scope names no home).
pub fn in_worker_scope(cgroup: &str) -> bool {
    cgroup.lines().any(|line| {
        line.rsplit(['/', ':'])
            .next()
            .is_some_and(|unit| unit.starts_with(WORKER_SCOPE_PREFIX) && unit.ends_with(".scope"))
    })
}

/// Who is calling. `chain` is `None` when the caller's process was not found.
pub fn classify(same_host: bool, chain: Option<&[Link]>) -> Caller {
    if !same_host {
        return Caller::Owner;
    }
    let Some(chain) = chain else {
        return Caller::Unseen;
    };
    match chain.iter().find(|l| l.mark == Mark::ThisHome) {
        Some(l) => Caller::Launched { pid: l.pid },
        None => Caller::Owner,
    }
}

/// Whether `caller` may make a call that needs `access`.
pub fn permit(caller: Caller, access: Access) -> Result<(), CallerError> {
    match (access, caller) {
        (Access::Read | Access::Stop, _) | (Access::Owner, Caller::Owner) => Ok(()),
        (Access::Owner, Caller::Launched { pid }) => Err(CallerError::Launched(pid)),
        (Access::Owner, Caller::Unseen) => Err(CallerError::Unseen),
    }
}

/// What a control action needs: only a resume starts work again.
pub fn control_access(action: ControlAction) -> Access {
    match action {
        ControlAction::Resume => Access::Owner,
        ControlAction::Pause | ControlAction::StopAll | ControlAction::Cancel => Access::Stop,
    }
}

/// True when the peer is on this host: a loopback address or one of this host's own
/// interface addresses, so a local caller cannot pass as remote by its source address.
pub fn same_host(peer: IpAddr, local_ips: &[IpAddr]) -> bool {
    let peer = peer.to_canonical();
    peer.is_loopback() || local_ips.iter().any(|ip| ip.to_canonical() == peer)
}

/// The pid of the client end of `peer` in `lsof -Fpfn` output: the `p` block with an `n`
/// line that starts with `<peer>->`. The other block holding the connection is the server.
pub fn peer_pid_from_lsof(text: &str, peer: SocketAddr) -> Option<u32> {
    let wanted = format!("{peer}->");
    let mut pid = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if line
            .strip_prefix('n')
            .is_some_and(|n| n.starts_with(&wanted))
        {
            return pid;
        }
    }
    None
}

/// The inode of the client socket in `/proc/net/tcp` or `/proc/net/tcp6` text: the row whose
/// local address is `peer` and whose remote address is `local`, this server.
pub fn socket_inode(text: &str, peer: SocketAddr, local: SocketAddr) -> Option<u64> {
    text.lines().skip(1).find_map(|row| {
        let cols: Vec<&str> = row.split_whitespace().collect();
        let (from, to) = (proc_addr(cols.get(1)?)?, proc_addr(cols.get(2)?)?);
        if from != peer || to != local {
            return None;
        }
        cols.get(9)?.parse().ok()
    })
}

/// One `/proc/net/tcp{,6}` address, `0100007F:E9CD`: the address as 32-bit words printed in
/// this host's byte order, then the port in hex.
fn proc_addr(text: &str) -> Option<SocketAddr> {
    let (addr, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let mut bytes = Vec::with_capacity(16);
    for i in (0..addr.len()).step_by(8) {
        let word = u32::from_str_radix(addr.get(i..i + 8)?, 16).ok()?;
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    let ip = match bytes.len() {
        4 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(bytes).ok()?)),
        16 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?)),
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_in_a_worker_scope_is_launched_whatever_its_environment() {
        // Red team case 2: `setsid` and `env -u` leave the ancestry and the marker, not the scope.
        let worker =
            "0::/user.slice/user-1001.slice/user@1001.service/app.slice/idlemax-w-7-1-55-0.scope\n";
        assert!(in_worker_scope(worker));
        assert!(!in_worker_scope(
            "0::/user.slice/user-1001.slice/session-3.scope\n"
        ));
        assert!(!in_worker_scope("0::/app.slice/idlemax-w-notes.service\n"));
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn link(pid: u32, mark: Mark) -> Link {
        Link { pid, mark }
    }

    #[test]
    fn mark_reads_the_marker_and_the_home() {
        let homes = [
            PathBuf::from("/home/me/.idlemax"),
            PathBuf::from("/data/me/.idlemax"),
        ];
        assert_eq!(mark_of(&env(&[("PATH", "/bin")]), &homes), Mark::None);
        assert_eq!(
            mark_of(&env(&[(LAUNCHED_ENV, "7")]), &homes),
            Mark::ThisHome
        );
        let this = env(&[(LAUNCHED_ENV, "7"), (HOME_ENV, "/home/me/.idlemax/")]);
        assert_eq!(mark_of(&this, &homes), Mark::ThisHome);
        let other = env(&[(LAUNCHED_ENV, "7"), (HOME_ENV, "/tmp/t/state")]);
        assert_eq!(mark_of(&other, &homes), Mark::OtherHome);
    }

    #[test]
    fn classify_walks_the_chain() {
        let behind = [
            link(30, Mark::None),
            link(20, Mark::None),
            link(10, Mark::ThisHome),
        ];
        assert_eq!(classify(true, Some(&behind)), Caller::Launched { pid: 10 });
        let other = [link(30, Mark::OtherHome), link(20, Mark::OtherHome)];
        assert_eq!(classify(true, Some(&other)), Caller::Owner);
        let plain = [link(30, Mark::None), link(1, Mark::None)];
        assert_eq!(classify(true, Some(&plain)), Caller::Owner);
        assert_eq!(classify(true, None), Caller::Unseen);
        assert_eq!(classify(false, None), Caller::Owner);
    }

    #[test]
    fn permit_lets_stop_through_and_holds_owner_calls() {
        let callers = [Caller::Owner, Caller::Launched { pid: 9 }, Caller::Unseen];
        for caller in callers {
            assert_eq!(permit(caller, Access::Stop), Ok(()));
        }
        assert_eq!(permit(Caller::Owner, Access::Owner), Ok(()));
        assert_eq!(
            permit(Caller::Launched { pid: 9 }, Access::Owner),
            Err(CallerError::Launched(9))
        );
        assert_eq!(
            permit(Caller::Unseen, Access::Owner),
            Err(CallerError::Unseen)
        );
        assert_eq!(control_access(ControlAction::Resume), Access::Owner);
        assert_eq!(control_access(ControlAction::Pause), Access::Stop);
    }

    #[test]
    fn same_host_knows_loopback_and_own_addresses() {
        let local: [IpAddr; 2] = ["10.0.0.5".parse().unwrap(), "192.168.1.9".parse().unwrap()];
        assert!(same_host("127.0.0.1".parse().unwrap(), &local));
        assert!(same_host("::ffff:127.0.0.1".parse().unwrap(), &local));
        assert!(same_host("192.168.1.9".parse().unwrap(), &local));
        assert!(!same_host("192.168.1.10".parse().unwrap(), &local));
    }

    // Copied from `lsof -nP -Fpfn -iTCP@127.0.0.1:52417` on Linux, a server (pid 1664057)
    // and one client (pid 1664059).
    const LSOF: &str = "p1664057\nf3\nn127.0.0.1:52417\nf4\nn127.0.0.1:52417->127.0.0.1:18812\n\
                        p1664059\nf3\nn127.0.0.1:18812->127.0.0.1:52417\n";

    #[test]
    fn lsof_gives_the_client_pid() {
        let peer = "127.0.0.1:18812".parse().unwrap();
        assert_eq!(peer_pid_from_lsof(LSOF, peer), Some(1664059));
        assert_eq!(peer_pid_from_lsof("", peer), None);
    }

    // Copied from /proc/net/tcp on Linux: a listener on 127.0.0.1:59853, the client end from
    // 127.0.0.1:36378 (inode 6496218), and the server's accepted end (inode 6496219).
    const PROC_TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   1: 0100007F:E9CD 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1001        0 6496217 1 0000000000000000 100 0 0 10 0
   8: 0100007F:8E1A 0100007F:E9CD 01 00000000:00000000 00:00000000 00000000  1001        0 6496218 2 0000000000000000 20 0 0 10 -1
  43: 0100007F:E9CD 0100007F:8E1A 01 00000000:00000000 00:00000000 00000000  1001        0 6496219 1 0000000000000000 20 0 0 10 -1
";

    #[test]
    fn proc_net_tcp_gives_the_client_inode() {
        let peer = "127.0.0.1:36378".parse().unwrap();
        let local = "127.0.0.1:59853".parse().unwrap();
        assert_eq!(socket_inode(PROC_TCP, peer, local), Some(6496218));
        let elsewhere = "127.0.0.1:36379".parse().unwrap();
        assert_eq!(socket_inode(PROC_TCP, elsewhere, local), None);
    }
}
