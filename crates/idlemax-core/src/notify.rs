//! Domain: job finish notification decisions. Pure.

use crate::types::JobStatus;

/// Job finish notification payload for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub message: String,
}

/// Formats the result string for display in notifications and listings.
pub fn job_result_label(status: JobStatus, exit_code: Option<i32>, error: Option<&str>) -> String {
    if let Some(err) = error {
        if !err.is_empty() {
            return format!("{status}: {err}");
        }
    }
    match exit_code {
        Some(code) if code != 0 => format!("{status} (exit {code})"),
        _ => status.to_string(),
    }
}

/// Builds a notification decision for a finished job.
pub fn job_finish_notification(
    name: &str,
    status: JobStatus,
    exit_code: Option<i32>,
    error: Option<&str>,
) -> Notification {
    let result = job_result_label(status, exit_code, error);
    Notification {
        title: format!("idlemax: {name}"),
        message: result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_success() {
        let n = job_finish_notification("echo test", JobStatus::Succeeded, Some(0), None);
        assert_eq!(n.title, "idlemax: echo test");
        assert_eq!(n.message, "succeeded");
    }

    #[test]
    fn format_failure_exit_code() {
        let n = job_finish_notification("build", JobStatus::Failed, Some(2), None);
        assert_eq!(n.title, "idlemax: build");
        assert_eq!(n.message, "failed (exit 2)");
    }

    #[test]
    fn format_failure_with_error() {
        let n = job_finish_notification(
            "fetch",
            JobStatus::Failed,
            Some(1),
            Some("connection timeout"),
        );
        assert_eq!(n.title, "idlemax: fetch");
        assert_eq!(n.message, "failed: connection timeout");
    }
}
