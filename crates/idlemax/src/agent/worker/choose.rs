//! Adapter: which host a worker launch uses. `workers.host`, the device profile and Herdr's
//! answer go through `pick_host`; a production device is always headless (A23) and a start
//! that fell back from Herdr says so in its first update.

use std::sync::Arc;

use idlemax_core::config::WorkerHostChoice;
use idlemax_core::types::DeviceProfile;
use idlemax_core::worker::launch::pick_host;

use super::host::{HostKind, WorkerHost};
use super::Workers;

/// The host picked for one launch, and the note that explains a fall back from Herdr.
pub(super) type Chosen = (HostKind, Arc<dyn WorkerHost>, Option<String>);

impl Workers {
    /// Adds the headless host, which `workers.host` and the profile may choose.
    pub fn with_headless(mut self, host: Arc<dyn WorkerHost>) -> Self {
        self.headless = Some(host);
        self
    }

    /// `workers.host`, which a reload may change, and the device profile read at start.
    pub fn set_host_policy(&mut self, setting: WorkerHostChoice, profile: Option<DeviceProfile>) {
        self.host_setting = setting;
        self.profile = profile;
    }

    /// The host for one launch (`pick_host`), with the note that explains a fall back from
    /// Herdr; Herdr is asked only when `auto` leaves the choice to its answer.
    pub(super) fn choose_host(&self) -> Result<Chosen, String> {
        let asks = self.host_setting == WorkerHostChoice::Auto
            && !matches!(self.profile, Some(DeviceProfile::Production));
        let answers = !asks || self.host.is_available();
        match pick_host(self.host_setting, self.profile, answers) {
            WorkerHostChoice::Herdr => Ok((HostKind::Herdr, Arc::clone(&self.host), None)),
            _ => {
                let host = self
                    .headless
                    .clone()
                    .ok_or("this system has no headless worker host")?;
                let note = (asks && !answers).then(|| "headless: herdr did not answer".into());
                Ok((HostKind::Headless, host, note))
            }
        }
    }
}
