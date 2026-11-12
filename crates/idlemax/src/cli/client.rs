//! Adapter: typed HTTP client for the coordinator API and the local agent API.

mod dispatch;
mod report;
mod schedules;

use std::time::Duration;

use idlemax_core::config::ReloadReport;
use idlemax_core::explain::{Explanation, WhySummary};
use idlemax_core::protocol::{ClusterStatus, ControlRequest, LocalStatus};
use idlemax_core::quota::QuotaView;
use idlemax_core::types::{Event, Job, JobId, JobSpec, JobStatus, Recommendation};
pub use report::Period;
use serde::de::DeserializeOwned;
use serde::Serialize;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot build HTTP client")]
    Build(#[source] reqwest::Error),
    /// Connection refused, timeout, DNS: the server is not there.
    #[error("cannot reach {url}")]
    Unreachable { url: String, source: reqwest::Error },
    /// Non-2xx with the server's `{"error": ...}` message.
    #[error("{path}: HTTP {status}: {message}")]
    Api {
        status: u16,
        path: String,
        message: String,
    },
    #[error("{path}: reading response")]
    Body {
        path: String,
        source: reqwest::Error,
    },
    #[error("{path}: unexpected response")]
    Decode {
        path: String,
        source: serde_json::Error,
    },
}

impl ClientError {
    /// This error and its causes on one line; Display alone shows only this layer.
    pub fn describe(&self) -> String {
        let mut out = self.to_string();
        let mut cause = std::error::Error::source(self);
        while let Some(c) = cause {
            out.push_str(&format!(": {c}"));
            cause = c.source();
        }
        out
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// One base URL plus bearer token. Coordinator methods and `local_*` methods
/// share the transport; pick the base URL accordingly.
#[derive(Clone)]
pub struct ApiClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl ApiClient {
    pub fn new(base: &str, token: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(ClientError::Build)?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn status(&self) -> Result<ClusterStatus> {
        self.get("/v1/status").await
    }

    /// GET /healthz, the one route that needs no token: the server is listening.
    pub async fn healthz(&self) -> Result<()> {
        let path = "/healthz";
        let resp = self
            .http
            .get(self.url(path))
            .send()
            .await
            .map_err(|source| ClientError::Unreachable {
                url: self.url(path),
                source,
            })?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|source| ClientError::Body {
            path: path.to_string(),
            source,
        })?;
        if status.is_success() {
            return Ok(());
        }
        Err(ClientError::Api {
            status: status.as_u16(),
            path: path.to_string(),
            message: error_message(&body),
        })
    }

    pub async fn jobs(&self, status: Option<JobStatus>, limit: u32) -> Result<Vec<Job>> {
        self.get(&jobs_path(status, limit)).await
    }

    pub async fn job(&self, id: JobId) -> Result<Job> {
        match self.get(&format!("/v1/jobs/{id}")).await {
            Ok(j) => Ok(j),
            Err(ClientError::Api { status: 404, .. }) => {
                let list = self.jobs(None, 200).await?;
                list.into_iter()
                    .find(|j| j.id == id)
                    .ok_or_else(|| ClientError::Api {
                        status: 404,
                        path: format!("/v1/jobs/{id}"),
                        message: format!("job {id} not found"),
                    })
            }
            Err(e) => Err(e),
        }
    }

    /// Queues the spec. The flag is false when the spec's key already named an unfinished
    /// job (HTTP 200), which is returned instead of a new one.
    pub async fn add_job(&self, spec: &JobSpec) -> Result<(Job, bool)> {
        let path = "/v1/jobs";
        let (status, job) = self
            .send_with_status(self.http.post(self.url(path)).json(spec), path)
            .await?;
        Ok((job, status == 201))
    }

    pub async fn cancel_job(&self, id: JobId) -> Result<Job> {
        self.post(&format!("/v1/jobs/{id}/cancel"), None::<&()>)
            .await
    }

    pub async fn why(&self, id: JobId) -> Result<Explanation> {
        self.get(&format!("/v1/jobs/{id}/why")).await
    }

    /// One headline reason per queued, leased or paused job, newest first.
    pub async fn why_summaries(&self, limit: u32) -> Result<Vec<WhySummary>> {
        self.get(&format!("/v1/why?limit={limit}")).await
    }

    pub async fn quota(&self) -> Result<Vec<QuotaView>> {
        self.get("/v1/quota").await
    }

    pub async fn control(&self, req: &ControlRequest) -> Result<ClusterStatus> {
        self.post("/v1/control", Some(req)).await
    }

    pub async fn events(&self, limit: u32) -> Result<Vec<Event>> {
        self.get(&format!("/v1/events?limit={limit}")).await
    }

    pub async fn advice(&self) -> Result<Vec<Recommendation>> {
        self.get("/v1/advice").await
    }

    pub async fn refresh_advice(&self) -> Result<Vec<Recommendation>> {
        self.post("/v1/advice/refresh", None::<&()>).await
    }

    pub async fn accept_advice(&self, id: i64) -> Result<Job> {
        self.post(&format!("/v1/advice/{id}/accept"), None::<&()>)
            .await
    }

    pub async fn dismiss_advice(&self, id: i64) -> Result<Recommendation> {
        self.post(&format!("/v1/advice/{id}/dismiss"), None::<&()>)
            .await
    }

    pub async fn local_status(&self) -> Result<LocalStatus> {
        self.get("/v1/local/status").await
    }

    pub async fn local_control(&self, req: &ControlRequest) -> Result<LocalStatus> {
        self.post("/v1/local/control", Some(req)).await
    }

    /// Asks the agent to re-read `config.toml` and apply what it can without a restart.
    pub async fn local_reload(&self) -> Result<ReloadReport> {
        self.post("/v1/local/reload", None::<&()>).await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(self.url(path)), path).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: Option<&B>,
    ) -> Result<T> {
        let mut req = self.http.post(self.url(path));
        if let Some(body) = body {
            req = req.json(body);
        }
        self.send(req, path).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn send<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        path: &str,
    ) -> Result<T> {
        Ok(self.send_with_status(req, path).await?.1)
    }

    /// Like `send`, with the HTTP status of the successful response.
    async fn send_with_status<T: DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
        path: &str,
    ) -> Result<(u16, T)> {
        let resp = req
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|source| ClientError::Unreachable {
                url: self.url(path),
                source,
            })?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(|source| ClientError::Body {
            path: path.to_string(),
            source,
        })?;
        if !status.is_success() {
            return Err(ClientError::Api {
                status: status.as_u16(),
                path: path.to_string(),
                message: error_message(&body),
            });
        }
        let value = serde_json::from_slice(&body).map_err(|source| ClientError::Decode {
            path: path.to_string(),
            source,
        })?;
        Ok((status.as_u16(), value))
    }
}

fn jobs_path(status: Option<JobStatus>, limit: u32) -> String {
    match status {
        Some(s) => format!("/v1/jobs?status={s}&limit={limit}"),
        None => format!("/v1/jobs?limit={limit}"),
    }
}

/// The `error` field of an ApiError body, else the raw text.
fn error_message(body: &[u8]) -> String {
    serde_json::from_slice::<idlemax_core::protocol::ApiError>(body)
        .map(|e| e.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(body).trim().to_string())
}
