//! Domain: which `Host` header values the HTTP servers answer to, and the network scope of
//! listen addresses (loopback, private network, or public). Pure.

use std::net::IpAddr;

/// The network reachability of an IP address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenScope {
    /// Loopback only: 127.0.0.0/8, ::1.
    Loopback,
    /// Private network: RFC 1918, RFC 6598 (tailnets), link-local, IPv6 unique local.
    Private,
    /// Publicly routable address.
    Public,
}

/// Classifies an IP address into loopback, private network, or public.
///
/// IPv4-mapped IPv6 addresses (e.g. `::ffff:127.0.0.1`) are canonicalized to IPv4 first.
pub fn listen_scope(ip: IpAddr) -> ListenScope {
    let ip = ip.to_canonical();
    if ip.is_loopback() {
        return ListenScope::Loopback;
    }
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // RFC 1918 private ranges, RFC 6598 shared address space (100.64.0.0/10),
            // or RFC 3927 link-local (169.254.0.0/16).
            let is_shared = octets[0] == 100 && (64..=127).contains(&octets[1]);
            if v4.is_private() || is_shared || v4.is_link_local() {
                ListenScope::Private
            } else {
                ListenScope::Public
            }
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            // RFC 4291 link-local (fe80::/10) or RFC 4193 unique local (fc00::/7).
            let is_link_local = (segments[0] & 0xffc0) == 0xfe80;
            let is_unique_local = (segments[0] & 0xfe00) == 0xfc00;
            if is_link_local || is_unique_local {
                ListenScope::Private
            } else {
                ListenScope::Public
            }
        }
    }
}

/// The extra host names a server answers to, besides IP literals and loopback names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedHosts {
    names: Vec<String>,
}

impl AllowedHosts {
    /// `extra` are host names (a port or a trailing dot is ignored); empty entries are dropped.
    pub fn new<I, S>(extra: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            names: extra
                .into_iter()
                .filter_map(|s| host_name(s.as_ref()))
                .collect(),
        }
    }

    /// True when the `Host` header value (`name`, `name:port`, `[v6]:port`) may be served.
    pub fn allows(&self, header: &str) -> bool {
        let Some(name) = host_name(header) else {
            return false;
        };
        name.parse::<IpAddr>().is_ok()
            || name == "localhost"
            || name.ends_with(".localhost")
            || self.names.contains(&name)
    }
}

/// The lowercase host of a `host[:port]` value: brackets and port removed, no trailing dot.
fn host_name(value: &str) -> Option<String> {
    let value = value.trim();
    let host = if let Some(rest) = value.strip_prefix('[') {
        rest.split_once(']')?.0
    } else if value.matches(':').count() > 1 {
        value
    } else {
        value.split(':').next()?
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_literals_loopback_names_and_listed_names_pass_and_every_other_name_does_not() {
        let hosts = AllowedHosts::new(["Server.Tailnet.TS.net", "box:7878", ""]);
        for yes in [
            "127.0.0.1:7878",
            "127.0.0.1",
            "[::1]:7878",
            "[::1]",
            "100.64.0.10:7878",
            "localhost:7878",
            "LOCALHOST.:7878",
            "app.localhost:9",
            "server.tailnet.ts.net:7878",
            "SERVER.TAILNET.TS.NET.",
            "box:1",
        ] {
            assert!(hosts.allows(yes), "{yes} should be allowed");
        }
        for no in [
            "evil.example:7878",
            "evil.example",
            // Rebinding domains that only look like something safe.
            "127.0.0.1.evil.example:7878",
            "localhost.evil.example",
            "notlocalhost",
            "server.tailnet.ts.net.evil.example",
            "user@evil.example",
            "evil.example:abc",
            "",
            "   ",
            ":7878",
            "[::1",
        ] {
            assert!(!hosts.allows(no), "{no:?} should be refused");
        }
        assert!(
            !AllowedHosts::default().allows("box:7878"),
            "nothing listed, no names"
        );
    }

    #[test]
    fn listen_scope_distinguishes_loopback_private_and_public() {
        for loopback in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            let ip: IpAddr = loopback.parse().unwrap();
            assert_eq!(listen_scope(ip), ListenScope::Loopback, "{loopback}");
        }

        for private in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.1.1",
            "fe80::1",
            "fd00::1",
            "::ffff:10.0.0.1",
            "::ffff:100.64.0.1",
        ] {
            let ip: IpAddr = private.parse().unwrap();
            assert_eq!(listen_scope(ip), ListenScope::Private, "{private}");
        }

        for public in [
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "2001:db8::1",
            "::ffff:192.0.2.1",
        ] {
            let ip: IpAddr = public.parse().unwrap();
            assert_eq!(listen_scope(ip), ListenScope::Public, "{public}");
        }
    }
}
