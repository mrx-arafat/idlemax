//! Adapter: the report route of the coordinator API.

use idlemax_core::report::Report;

use super::{ApiClient, Result};

/// The period a report covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    /// The last N days; the coordinator names it.
    Days(i64),
    /// From a Unix time to now, with a label made of letters only (`today`).
    Since { since_ms: i64, label: &'static str },
}

impl ApiClient {
    pub async fn report(&self, period: Period) -> Result<Report> {
        let path = match period {
            Period::Days(n) => format!("/v1/report?days={n}"),
            Period::Since { since_ms, label } => {
                format!("/v1/report?since_ms={since_ms}&label={label}")
            }
        };
        self.get(&path).await
    }
}
