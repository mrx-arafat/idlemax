//! Delivery: `idlemax why` output and the warning `idlemax job add` prints when no device
//! would run the new job.

use idlemax_core::explain::{allowlist_warning, Explanation, Reason};
use idlemax_core::types::{DeviceInfo, JobSpec};

use super::rows::rel;
use super::ApiClient;

/// The explanation as text: the summary, what blocks every device, then each device.
pub fn render(e: &Explanation) -> String {
    let mut text = e.summary.clone();
    if !e.reasons.is_empty() {
        text.push_str("\n\nBlocked on every device:");
        push_reasons(&mut text, &e.reasons);
    }
    let mut clear = Vec::new();
    for d in &e.devices {
        let state = format!(
            "{}, heartbeat {}",
            if d.online { "online" } else { "offline" },
            rel(d.last_seen_ms, e.at_ms)
        );
        if d.ready {
            text.push_str(&format!("\n\n{} ({state}): would take it now", d.device));
        } else if d.reasons.is_empty() {
            clear.push(d.device.as_str());
        } else {
            text.push_str(&format!("\n\n{} ({state}):", d.device));
            push_reasons(&mut text, &d.reasons);
        }
    }
    if !clear.is_empty() {
        text.push_str(&format!(
            "\n\nNo device-specific blocker: {}",
            clear.join(", ")
        ));
    }
    text
}

fn push_reasons(text: &mut String, reasons: &[Reason]) {
    for r in reasons {
        text.push_str(&format!("\n  - {}", r.message));
        if let Some(fix) = &r.fix {
            text.push_str(&format!("\n    fix: {fix}"));
        }
    }
}

/// Warns on stderr when every device that could run `spec` has reported an allowlist
/// without its command. Best effort: it never fails the add.
pub async fn warn_if_unrunnable(c: &ApiClient, spec: &JobSpec) {
    if let Ok(status) = c.status().await {
        let devices: Vec<DeviceInfo> = status.devices.into_iter().map(|d| d.info).collect();
        if let Some(warning) = allowlist_warning(spec, &devices) {
            eprintln!("warning: {warning}");
        }
    }
}
