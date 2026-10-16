//! The Android VM as the session's guest launcher.

use std::time::Duration;

use dsa_host_core::session::GuestLauncher;

use crate::guid::Guid;
use crate::hcs::ComputeSystem;
use crate::vm::AndroidVmConfig;

/// How long a single HCS call (create, start, terminate) may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the VM may take to exit after it is terminated.
const TERMINATE_GRACE: Duration = Duration::from_secs(10);

/// Starts and stops the Android VM.
///
/// Stopping first asks the guest to shut down, and stops the VM outright if it has not
/// exited within the stop timeout. Either way every guest process has ended once `stop`
/// returns `Ok`, because the VM itself is gone.
pub struct AndroidVmLauncher {
    id: String,
    config: AndroidVmConfig,
    system: Option<ComputeSystem>,
}

impl AndroidVmLauncher {
    /// `id` names the compute system in HCS; it must be unique on the machine.
    pub fn new(id: &str, config: AndroidVmConfig) -> AndroidVmLauncher {
        AndroidVmLauncher {
            id: id.to_string(),
            config,
            system: None,
        }
    }

    /// The running VM's ID, which the control channel's Hyper-V sockets are bound to.
    pub fn runtime_id(&self) -> Result<Guid, String> {
        let system = self
            .system
            .as_ref()
            .ok_or("the Android VM is not running")?;
        system.runtime_id(CALL_TIMEOUT).map_err(|e| e.to_string())
    }
}

impl GuestLauncher for AndroidVmLauncher {
    fn start(&mut self) -> Result<(), String> {
        if self.system.is_some() {
            return Err("the Android VM is already running".into());
        }
        let document = self
            .config
            .to_hcs_document()
            .map_err(|e| e.to_string())?
            .to_string();
        let system =
            ComputeSystem::create(&self.id, &document, CALL_TIMEOUT).map_err(|e| e.to_string())?;
        // On failure the system is dropped, which closes the handle and removes the VM.
        system.start(CALL_TIMEOUT).map_err(|e| e.to_string())?;
        self.system = Some(system);
        Ok(())
    }

    fn stop(&mut self, timeout: Duration) -> Result<(), String> {
        let Some(system) = self.system.as_ref() else {
            return Ok(());
        };
        // A guest that cannot take the request (still booting, or hung) is terminated below.
        let asked = system.request_shutdown(timeout.min(CALL_TIMEOUT));
        if asked.is_err() || !system.wait_exited(timeout) {
            system.terminate(CALL_TIMEOUT).map_err(|e| e.to_string())?;
            if !system.wait_exited(TERMINATE_GRACE) {
                return Err(format!(
                    "the Android VM did not exit within {} s of being terminated",
                    TERMINATE_GRACE.as_secs()
                ));
            }
        }
        self.system = None;
        Ok(())
    }
}
