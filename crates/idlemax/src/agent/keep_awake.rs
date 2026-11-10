//! Adapter: owns the macOS idle-sleep assertion child and reaps it on release.

use tokio::process::Child;

#[derive(Default)]
pub(super) struct Assertion {
    child: Option<Child>,
}

impl Assertion {
    /// Apply the pure decision; report actual state if the OS helper cannot start.
    pub(super) async fn apply(&mut self, hold: bool) -> std::io::Result<()> {
        if let Some(child) = &mut self.child {
            if child.try_wait()?.is_some() {
                self.child = None;
            }
        }
        if !hold {
            if let Some(mut child) = self.child.take() {
                child.kill().await?;
            }
            return Ok(());
        }
        if self.child.is_none() {
            self.child = Some(spawn()?);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn spawn() -> std::io::Result<Child> {
    use std::process::Stdio;

    tokio::process::Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

#[cfg(not(target_os = "macos"))]
fn spawn() -> std::io::Result<Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "keep-awake is supported on macOS only",
    ))
}
