//! Domain: job finish notification decisions and AppleScript rendering. Pure.

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

/// Renders AppleScript for macOS `display notification`.
pub fn render_applescript(notif: &Notification) -> String {
    let title = escape_applescript(&notif.title);
    let msg = escape_applescript(&notif.message);
    format!("display notification \"{msg}\" with title \"{title}\"")
}

/// Escapes characters for an AppleScript string literal.
pub fn escape_applescript(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_success() {
        let n = job_finish_notification("echo test", JobStatus::Succeeded, Some(0), None);
        assert_eq!(n.title, "idlemax: echo test");
        assert_eq!(n.message, "succeeded");
        assert_eq!(
            render_applescript(&n),
            "display notification \"succeeded\" with title \"idlemax: echo test\""
        );
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
        assert_eq!(n.message, "failed: connection timeout");
    }

    #[test]
    fn escape_quotes_and_backslashes() {
        let n = Notification {
            title: "idlemax: \"quote\" & \\slash\\".into(),
            message: "line 1\nline 2".into(),
        };
        let script = render_applescript(&n);
        assert_eq!(
            script,
            "display notification \"line 1\\nline 2\" with title \"idlemax: \\\"quote\\\" & \\\\slash\\\\\""
        );
    }
}
