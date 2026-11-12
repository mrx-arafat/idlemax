//! Delivery: turns any error into `error:` and `help:` lines plus an exit code.
//! Exit codes: 0 success and no-ops, 1 runtime error, 2 usage error.

use super::toon::{self, Node};
use super::Format;
use crate::cli::ClientError;

pub const RUNTIME: u8 = 1;
pub const USAGE: u8 = 2;

/// What to do about an owner-only command refused to a launched process.
pub const OWNER_ONLY_HELP: &str = "ask the owner to run it from their own terminal";

/// What went wrong and the command that fixes it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{what}")]
pub struct CliError {
    pub what: String,
    pub help: Option<String>,
    pub code: u8,
}

/// Which server a request was meant for, to word the "not running" error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    Coordinator,
    Agent,
}

impl CliError {
    pub fn usage(what: impl Into<String>, help: impl Into<String>) -> Self {
        Self::new(what, Some(help.into()), USAGE)
    }

    pub fn runtime(what: impl Into<String>, help: impl Into<String>) -> Self {
        Self::new(what, Some(help.into()), RUNTIME)
    }

    fn node(&self) -> Node {
        let mut fields = vec![("error".to_string(), Node::Str(self.what.clone()))];
        if let Some(h) = &self.help {
            fields.push(("help".to_string(), Node::Str(h.clone())));
        }
        Node::Obj(fields)
    }

    fn json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("error".into(), self.what.as_str().into());
        if let Some(h) = &self.help {
            map.insert("help".into(), h.as_str().into());
        }
        map.into()
    }

    fn new(what: impl Into<String>, help: Option<String>, code: u8) -> Self {
        Self {
            what: what.into(),
            help,
            code,
        }
    }

    /// The error shown for `e`: an explicit [`CliError`], a mapped API failure, or the
    /// plain message chain.
    pub fn from_error(e: &anyhow::Error) -> Self {
        if let Some(c) = e.downcast_ref::<CliError>() {
            return c.clone();
        }
        match e.downcast_ref::<ClientError>() {
            Some(c) => from_client(c),
            None => Self::new(crate::error_text::render(e), None, RUNTIME),
        }
    }

    pub fn render(&self, format: Format) -> String {
        match format {
            Format::Toon => toon::encode(&self.node()),
            Format::Json => {
                serde_json::to_string_pretty(&self.json()).unwrap_or_else(|_| self.what.clone())
            }
            Format::Table => {
                let mut text = format!("error: {}", self.what);
                if let Some(h) = &self.help {
                    text.push_str(&format!("\nhelp: {h}"));
                }
                text
            }
        }
    }
}

/// The error for a server that does not answer.
pub fn unreachable(peer: Peer, url: &str) -> CliError {
    let origin = reqwest::Url::parse(url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|_| url.to_string());
    match peer {
        Peer::Coordinator => CliError::runtime(
            format!("coordinator unreachable at {origin}"),
            "idlemax coord (or pass --coord <URL> for a running one); `idlemax local stop-all` works without it",
        ),
        Peer::Agent => CliError::runtime(
            format!("local agent unreachable at {origin}"),
            "idlemax agent (is it running?); touch $IDLEMAX_HOME/PAUSE pauses it without the API",
        ),
    }
}

fn from_client(e: &ClientError) -> CliError {
    match e {
        ClientError::Api {
            status: 403,
            message,
            ..
        } => CliError::runtime(message.clone(), OWNER_ONLY_HELP),
        ClientError::Api { status: 401, .. } => CliError::runtime(
            "the server rejected this device's API token",
            "copy the coordinator's token to $IDLEMAX_HOME/token, or set IDLEMAX_TOKEN",
        ),
        ClientError::Api {
            status: 404,
            path,
            message,
        } => CliError {
            what: message.clone(),
            help: list_command(path).map(str::to_string),
            code: RUNTIME,
        },
        ClientError::Api {
            status, message, ..
        } if *status >= 500 => {
            CliError::new(format!("coordinator error: {message}"), None, RUNTIME)
        }
        ClientError::Api { message, .. } => CliError::new(message.clone(), None, RUNTIME),
        other => CliError::new(other.describe(), None, RUNTIME),
    }
}

/// The command that lists what a missing id should have been.
fn list_command(path: &str) -> Option<&'static str> {
    [
        ("/v1/jobs", "idlemax jobs"),
        ("/v1/advice", "idlemax advise"),
        ("/v1/schedules", "idlemax schedule list"),
    ]
    .into_iter()
    .find_map(|(prefix, cmd)| path.starts_with(prefix).then_some(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, path: &str, message: &str) -> anyhow::Error {
        anyhow::Error::new(ClientError::Api {
            status,
            path: path.into(),
            message: message.into(),
        })
    }

    #[test]
    fn api_failures_name_the_fix() {
        let missing = CliError::from_error(&api(404, "/v1/jobs/9/cancel", "job 9 not found"));
        assert_eq!(missing.what, "job 9 not found");
        assert_eq!(missing.help.as_deref(), Some("idlemax jobs"));
        assert_eq!(missing.code, RUNTIME);

        let denied = CliError::from_error(&api(401, "/v1/status", "missing or wrong bearer token"));
        assert!(denied.what.contains("token") && denied.help.is_some());

        let plain = CliError::from_error(&anyhow::anyhow!("reading x").context("outer"));
        assert_eq!(plain.what, "outer: reading x");
        assert_eq!(plain.help, None);
    }

    #[test]
    fn an_explicit_error_keeps_its_code_through_context() {
        let e = anyhow::Error::new(CliError::usage("--fields: no field `x`", "valid: a, b"))
            .context("while listing");
        let c = CliError::from_error(&e);
        assert_eq!((c.code, c.help.as_deref()), (USAGE, Some("valid: a, b")));
    }

    #[test]
    fn errors_render_in_the_active_format() {
        let c = CliError::runtime("cannot reach http://x:1", "idlemax coord");
        assert_eq!(
            c.render(Format::Toon),
            "error: \"cannot reach http://x:1\"\nhelp: idlemax coord"
        );
        assert_eq!(
            c.render(Format::Table),
            "error: cannot reach http://x:1\nhelp: idlemax coord"
        );
        let json: serde_json::Value = serde_json::from_str(&c.render(Format::Json)).unwrap();
        assert_eq!(json["error"], "cannot reach http://x:1");
        assert_eq!(json["help"], "idlemax coord");
    }

    #[test]
    fn the_unreachable_error_shows_the_origin_not_the_path() {
        let e = unreachable(Peer::Coordinator, "http://127.0.0.1:7878/v1/status");
        assert_eq!(e.what, "coordinator unreachable at http://127.0.0.1:7878");
        assert!(e.help.unwrap().starts_with("idlemax coord"));
    }
}
