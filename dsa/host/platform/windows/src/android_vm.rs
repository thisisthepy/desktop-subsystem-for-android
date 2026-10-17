//! The Android VM as the session's guest launcher.

use std::time::Duration;

use dsa_host_core::session::GuestLauncher;

use crate::guid::Guid;
use crate::hcs::ComputeSystem;
use crate::vm::AndroidVmConfig;
use crate::vm_listener::VmIdSlot;

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
    vm_id: VmIdSlot,
}

impl AndroidVmLauncher {
    /// `id` names the compute system in HCS; it must be unique on the machine.
    pub fn new(id: &str, config: AndroidVmConfig) -> AndroidVmLauncher {
        AndroidVmLauncher {
            id: id.to_string(),
            config,
            system: None,
            vm_id: VmIdSlot::default(),
        }
    }

    /// Holds the running VM's ID, for the control channel's listener to bind to. It is set
    /// once the VM has started and cleared once it has stopped.
    pub fn vm_id_slot(&self) -> VmIdSlot {
        VmIdSlot::clone(&self.vm_id)
    }

    /// The running VM's ID, which the control channel's Hyper-V sockets are bound to.
    pub fn runtime_id(&self) -> Result<Guid, String> {
        self.vm_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ok_or_else(|| "the Android VM is not running".to_string())
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
        // On any failure below the system is dropped, which closes the handle and so
        // removes the VM.
        system.start(CALL_TIMEOUT).map_err(|e| e.to_string())?;
        // Without its ID nothing can reach the VM, so a VM whose ID cannot be read is of no
        // use.
        let vm_id = system
            .runtime_id(CALL_TIMEOUT)
            .map_err(|e| format!("the Android VM started but its ID could not be read: {e}"))?;
        *self.vm_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(vm_id);
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
        *self.vm_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}
