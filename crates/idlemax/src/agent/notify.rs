//! Adapter: sends desktop notifications on macOS via osascript.

use idlemax_core::notify;
use idlemax_core::types::JobStatus;
use tracing::warn;

/// Sends a desktop notification for a finished job if running on macOS.
pub fn notify_job_finish(
    name: &str,
    status: JobStatus,
    exit_code: Option<i32>,
    error: Option<&str>,
) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let notif = notify::job_finish_notification(name, status, exit_code, error);
    let script = notify::render_applescript(&notif);
    match std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .spawn()
    {
        Ok(_) => {}
        Err(e) => warn!(error = %e, "cannot spawn osascript notification"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_does_not_panic() {
        notify_job_finish("test-job", JobStatus::Succeeded, Some(0), None);
    }
}
