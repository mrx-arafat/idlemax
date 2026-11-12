//! Adapter: the caller check of the coordinator and the agent local API. An owner-only call
//! looks up the calling process and its ancestors, and the pure rule in
//! `idlemax_core::control::caller` decides; `Read` and `Stop` calls never run the lookup.

mod lookup;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use idlemax_core::control::caller::{classify, permit, Access, Link};

use super::http::ApiFailure;
use crate::coord::server::Conn;

/// How long an owner-only call waits for the lookup before it counts the caller as unseen.
const LOOKUP_LIMIT: Duration = Duration::from_secs(2);

/// What the lookup found about one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub same_host: bool,
    /// The caller first, then its ancestors; `None` when the process was not found.
    pub chain: Option<Vec<Link>>,
}

/// Finds the process behind a connection; `homes` is this server's state dir.
pub type Lookup = fn(Conn, &[PathBuf]) -> Seen;

/// Built once per server and carried to the handlers as an `Extension`.
#[derive(Clone)]
pub struct CallerCheck {
    homes: Arc<[PathBuf]>,
    lookup: Lookup,
}

impl CallerCheck {
    /// The check of the server whose state dir is `root`, with the OS lookup.
    pub fn for_home(root: &Path) -> Self {
        Self::with_lookup(root, lookup::find)
    }

    pub fn with_lookup(root: &Path, lookup: Lookup) -> Self {
        let mut homes = vec![root.to_path_buf()];
        if let Ok(real) = std::fs::canonicalize(root) {
            if real != root {
                homes.push(real);
            }
        }
        Self {
            homes: homes.into(),
            lookup,
        }
    }

    /// Ok when the caller on `conn` may make a call that needs `access`, else a 403.
    pub async fn ensure(&self, conn: Conn, access: Access) -> Result<(), ApiFailure> {
        if access != Access::Owner {
            return Ok(());
        }
        let (homes, lookup) = (Arc::clone(&self.homes), self.lookup);
        let task = tokio::task::spawn_blocking(move || lookup(conn, &homes));
        let seen = match tokio::time::timeout(LOOKUP_LIMIT, task).await {
            Ok(Ok(seen)) => seen,
            Ok(Err(_)) | Err(_) => {
                tracing::warn!(peer = %conn.peer, "the caller lookup failed or took too long");
                Seen {
                    same_host: true,
                    chain: None,
                }
            }
        };
        let caller = classify(seen.same_host, seen.chain.as_deref());
        permit(caller, access).map_err(|e| ApiFailure::Forbidden(e.to_string()))
    }
}

/// The handler side: the connection and the server's check, taken from the request.
pub struct Who {
    conn: Option<Conn>,
    check: Option<CallerCheck>,
}

impl Who {
    pub async fn ensure(&self, access: Access) -> Result<(), ApiFailure> {
        match (&self.check, self.conn) {
            (Some(check), Some(conn)) => check.ensure(conn, access).await,
            _ if access != Access::Owner => Ok(()),
            _ => Err(ApiFailure::Internal(
                "a request without its connection or caller check".into(),
            )),
        }
    }
}

#[cfg(test)]
impl Who {
    /// A caller on another host, judged by the token alone: for handlers called directly.
    pub fn remote() -> Self {
        let conn = Conn {
            peer: ([192, 0, 2, 1], 50000).into(),
            local: ([192, 0, 2, 2], 7878).into(),
        };
        let lookup: Lookup = |_, _| Seen {
            same_host: false,
            chain: None,
        };
        Self {
            conn: Some(conn),
            check: Some(CallerCheck::with_lookup(Path::new("/"), lookup)),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Who {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self {
            conn: parts.extensions.get::<Conn>().copied(),
            check: parts.extensions.get::<CallerCheck>().cloned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use idlemax_core::control::caller::Mark;
    use idlemax_core::protocol::ControlRequest;
    use idlemax_core::types::{ControlAction, ControlScope};
    use idlemax_core::Config;
    use reqwest::StatusCode;

    use super::super::tests::Api;
    use super::*;

    fn conn() -> Conn {
        Conn {
            peer: "127.0.0.1:50000".parse().unwrap(),
            local: "127.0.0.1:7878".parse().unwrap(),
        }
    }

    fn check(lookup: Lookup) -> CallerCheck {
        CallerCheck::with_lookup(Path::new("/nonexistent/idlemax"), lookup)
    }

    #[tokio::test]
    async fn a_launched_ancestor_is_refused_owner_calls_but_may_stop() {
        let marked = check(|_, _| Seen {
            same_host: true,
            chain: Some(vec![
                Link {
                    pid: 30,
                    mark: Mark::None,
                },
                Link {
                    pid: 20,
                    mark: Mark::ThisHome,
                },
            ]),
        });
        let refused = marked.ensure(conn(), Access::Owner).await.unwrap_err();
        assert!(matches!(&refused, ApiFailure::Forbidden(t) if t.contains("(pid 20)")));
        assert!(marked.ensure(conn(), Access::Stop).await.is_ok());
    }

    #[tokio::test]
    async fn an_unseen_local_caller_cannot_resume_but_can_stop_everything() {
        let unseen: Lookup = |_, _| Seen {
            same_host: true,
            chain: None,
        };
        let api = Api::start_checked(Config::default(), Some(unseen)).await;
        let request = |action| ControlRequest {
            action,
            scope: ControlScope::Global,
            reason: None,
        };
        let refused = api
            .post("/v1/control", &request(ControlAction::Resume))
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert!(refused
            .text()
            .await
            .unwrap()
            .contains("could not find the program"));
        let stop = api
            .post("/v1/control", &request(ControlAction::StopAll))
            .await;
        assert_eq!(stop.status(), StatusCode::OK);
    }
}
