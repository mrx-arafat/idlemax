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
    match std::process::Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 1 of argv) with title (item 2 of argv)",
            "-e",
            "end run",
        ])
        .arg(&notif.message)
        .arg(&notif.title)
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => warn!(error = %e, "cannot spawn osascript notification"),
    }
}
