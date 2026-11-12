//! Domain: the desktop notification payload, the result label of a finished job, and the
//! notices that interrupt the owner (`notice`). Pure.

use crate::types::JobStatus;

pub mod notice;

/// A desktop notification.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_success() {
        assert_eq!(
            job_result_label(JobStatus::Succeeded, Some(0), None),
            "succeeded"
        );
    }

    #[test]
    fn format_failure_exit_code() {
        assert_eq!(
            job_result_label(JobStatus::Failed, Some(2), None),
            "failed (exit 2)"
        );
    }

    #[test]
    fn format_failure_with_error() {
        let label = job_result_label(JobStatus::Failed, Some(1), Some("connection timeout"));
        assert_eq!(label, "failed: connection timeout");
    }
}
