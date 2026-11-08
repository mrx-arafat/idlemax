//! Domain: which `Host` header values the HTTP servers answer to. A web page can be made to
//! reach a local server under the attacker's own domain name (DNS rebinding), and the browser
//! then sends that name as `Host`. Only IP literals, loopback names and names the owner lists
//! are accepted. Pure.

use std::net::IpAddr;

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
}
