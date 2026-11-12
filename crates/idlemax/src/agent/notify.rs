//! Adapter: shows a desktop notification on macOS via osascript; other systems get the inbox
//! note only.

use idlemax_core::notify::Notification;
use tracing::warn;

pub fn show(notif: &Notification) {
    if !cfg!(target_os = "macos") {
        return;
    }
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
