//! Coordinator HTTP client for the agent. Adapter layer: JSON over HTTP, bearer auth.

use std::time::Duration;

use idlemax_core::protocol::{Heartbeat, HeartbeatReply, JobUpdate};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Whole-request cap, so a hung coordinator never stalls the agent loop for long.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest slice of a non-JSON error body kept in the error message.
const ERROR_BODY_CHARS: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("build http client: {0}")]
    Build(#[source] reqwest::Error),
    #[error("POST {url}: {source}")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("POST {url}: HTTP {status}: {message}")]
    Status {
        url: String,
        status: u16,
        message: String,
    },
    #[error("POST {url}: bad reply: {source}")]
    Decode {
        url: String,
        #[source]
        source: serde_json::Error,
    },
}

impl ClientError {
    /// The coordinator refused this request for good (bad or stale data); retrying the
    /// same body cannot succeed. Auth, timeout and rate-limit answers stay retryable.
    pub fn is_rejected(&self) -> bool {
        matches!(
            self,
            ClientError::Status {
                status: 400..=499,
                ..
            }
        ) && !matches!(
            self,
            ClientError::Status {
                status: 401 | 403 | 408 | 429,
                ..
            }
        )
    }
}

/// Talks to the coordinator. Cheap to clone.
#[derive(Debug, Clone)]
pub struct CoordClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl CoordClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self, ClientError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // The coordinator is on the tailnet or loopback; never route it via a proxy.
            .no_proxy()
            .build()
            .map_err(ClientError::Build)?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        })
    }

    pub async fn heartbeat(&self, hb: &Heartbeat) -> Result<HeartbeatReply, ClientError> {
        self.post("/v1/agent/heartbeat", hb).await
    }

    pub async fn job_update(&self, update: &JobUpdate) -> Result<(), ClientError> {
        let _: serde_json::Value = self.post("/v1/agent/job-update", update).await?;
        Ok(())
    }

    async fn post<B, R>(&self, path: &str, body: &B) -> Result<R, ClientError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let url = format!("{}{path}", self.base_url);
        let transport = |source| ClientError::Transport {
            url: url.clone(),
            source,
        };
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(transport)?;
        if !status.is_success() {
            let message = error_message(&bytes);
            return Err(ClientError::Status {
                url,
                status: status.as_u16(),
                message,
            });
        }
        serde_json::from_slice(&bytes).map_err(|source| ClientError::Decode { url, source })
    }
}

/// `{"error": "..."}` from the API, else the start of the raw body.
fn error_message(body: &[u8]) -> String {
    serde_json::from_slice::<idlemax_core::protocol::ApiError>(body)
        .map(|e| e.error)
        .unwrap_or_else(|_| {
            String::from_utf8_lossy(body)
                .chars()
                .take(ERROR_BODY_CHARS)
                .collect()
        })
}
