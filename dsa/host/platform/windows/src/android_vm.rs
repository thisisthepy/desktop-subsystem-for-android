//! The Android VM as the session's guest launcher.
//!
//! With a console log, each boot's serial console is kept in that file (see
//! `console_pipe`), the session hears at once when the VM exits during boot, and a start
//! that fails reports the console's last lines.
//!
//! Each boot's secret reaches the guest as a bootconfig appended to a copy of the image's
//! initrd. The copy exists only while the VM starts: the VM has read it by the time it runs,
//! and it is deleted then.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dsa_host_core::bootconfig;
use dsa_host_core::session::GuestLauncher;
use dsa_protocol::auth::{bootconfig_entry, BootSecret};

use crate::console_log::{boot_header, ConsoleLog};
use crate::console_pipe::ConsoleCapture;
use crate::guid::Guid;
use crate::hcs::{grant_vm_access, ComputeSystem};
use crate::vm::AndroidVmConfig;
use crate::vm_listener::VmIdSlot;

/// How long a single HCS call (create, start, terminate) may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the VM may take to exit after it is terminated.
const TERMINATE_GRACE: Duration = Duration::from_secs(10);
/// How long a stopped VM's console may take to deliver its last output.
const CONSOLE_DRAIN: Duration = Duration::from_secs(2);

/// Starts and stops the Android VM.
///
/// Stopping first asks the guest to shut down, and stops the VM outright if it has not
/// exited within the stop timeout. Either way every guest process has ended once `stop`
/// returns `Ok`, because the VM itself is gone.
/// Supplies the VM's configuration each time it starts.
pub type ConfigSource = Box<dyn FnMut() -> Result<AndroidVmConfig, String> + Send>;

pub struct AndroidVmLauncher {
    id: String,
    config: ConfigSource,
    system: Option<ComputeSystem>,
    vm_id: VmIdSlot,
    /// Where each boot's console is kept. Without it the VM gets no serial port.
    console_log: Option<PathBuf>,
    /// The running boot's console.
    console: Option<ConsoleCapture>,
    /// What the last finished boot's console says, for a start that failed.
    console_report: Option<String>,
    /// Where each boot's initrd, with its secret, is written while the VM starts.
    boot_dir: PathBuf,
}

/// A file that is removed when this is dropped.
struct TransientFile(PathBuf);

impl Drop for TransientFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes `initrd` with `secret` appended as bootconfig to `path`, readable only by the
/// account the host runs as until the VM is let in.
fn write_boot_initrd(
    initrd: &Path,
    secret: &BootSecret,
    path: &Path,
) -> Result<TransientFile, String> {
    let image = std::fs::read(initrd)
        .map_err(|e| format!("could not read the initrd {}: {e}", initrd.display()))?;
    let file = TransientFile(path.to_path_buf());
    std::fs::write(path, bootconfig::append(&image, &bootconfig_entry(secret)))
        .map_err(|e| format!("could not write the boot's initrd {}: {e}", path.display()))?;
    Ok(file)
}

impl AndroidVmLauncher {
    /// `id` names the compute system in HCS; it must be unique on the machine.
    pub fn new(id: &str, config: AndroidVmConfig) -> AndroidVmLauncher {
        AndroidVmLauncher::with_source(id, Box::new(move || Ok(config.clone())))
    }

    /// Like [`new`](Self::new), but asks `config` for the configuration at every start, so
    /// that a VM started after an update boots the image current then.
    pub fn with_source(id: &str, config: ConfigSource) -> AndroidVmLauncher {
        AndroidVmLauncher {
            id: id.to_string(),
            config,
            system: None,
            vm_id: VmIdSlot::default(),
            console_log: None,
            console: None,
            console_report: None,
            boot_dir: std::env::temp_dir(),
        }
    }

    /// Writes each boot's initrd into `dir` instead of the temporary folder.
    pub fn with_boot_dir(mut self, dir: PathBuf) -> AndroidVmLauncher {
        self.boot_dir = dir;
        self
    }

    /// Keeps every boot's serial console in the file at `path`, and reports its last lines
    /// when a start fails.
    pub fn with_console_log(mut self, path: PathBuf) -> AndroidVmLauncher {
        self.console_log = Some(path);
        self
    }

    /// Serves the console pipe `config` names and starts the boot's entry in the log.
    /// Returns the pipe to give the VM, or `None` when the console cannot be kept this time:
    /// the VM then boots without a serial port rather than not at all.
    fn begin_console(&mut self, config: &AndroidVmConfig) -> Option<String> {
        let (Some(pipe), Some(path)) = (&config.console_pipe, &self.console_log) else {
            return None;
        };
        let header = boot_header(
            SystemTime::now(),
            config.image_version.as_deref(),
            &config.full_kernel_cmdline(),
        );
        match ConsoleCapture::start(pipe, ConsoleLog::begin_boot(path, &header)) {
            Ok(capture) => {
                self.console = Some(capture);
                Some(pipe.clone())
            }
            Err(failed) => {
                let (error, mut log) = *failed;
                let why = format!("the Android console is not kept for this boot: {error}");
                eprintln!("{}: {why}", self.id);
                log.note(&why);
                self.console_report = Some(format!("{why} (log: {})", path.display()));
                None
            }
        }
    }

    /// Ends the console capture, keeping what it says for [`diagnostics`](GuestLauncher::diagnostics).
    fn end_console(&mut self, drain: Duration) {
        if let Some(mut console) = self.console.take() {
            if !console.finish(drain) {
                eprintln!(
                    "{}: the Android console's reader did not end; it ends by itself once \
                     the VM closes the pipe",
                    self.id
                );
            }
            self.console_report = Some(console.failure_report());
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
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        if self.system.is_some() {
            return Err("the Android VM is already running".into());
        }
        // A previous boot's console is finished by its stop; this is only a safety net, so
        // the pipe's name is free again.
        self.end_console(Duration::ZERO);
        self.console_report = None;
        let mut config = (self.config)()?;
        let Some(initrd) = config.initrd.clone() else {
            return Err(
                "the Android VM has no initrd, which is what carries the boot's secret to \
                 the guest"
                    .into(),
            );
        };
        // Removed on every way out of this function: by then the VM has read it or will
        // never run.
        let boot_initrd = write_boot_initrd(
            &initrd,
            secret,
            &self.boot_dir.join(format!("{}.initrd", self.id)),
        )?;
        config.initrd = Some(boot_initrd.0.clone());
        config.kernel_cmdline = format!("{} {}", config.kernel_cmdline, bootconfig::CMDLINE_FLAG);
        config.console_pipe = self.begin_console(&config);
        let document = config
            .to_hcs_document()
            .map_err(|e| e.to_string())?
            .to_string();
        let system =
            ComputeSystem::create(&self.id, &document, CALL_TIMEOUT).map_err(|e| e.to_string())?;
        // The VM opens its disk when it starts. A disk in the user's profile admits only the
        // user, SYSTEM and administrators, so the VM is let in to that one file first.
        for path in [&config.data_disk, &boot_initrd.0] {
            grant_vm_access(&self.id, path).map_err(|e| {
                format!(
                    "the Android VM could not be given access to {}: {e}",
                    path.display()
                )
            })?;
        }
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
            // A start that failed part of the way may have served the console already.
            self.end_console(CONSOLE_DRAIN);
            return Ok(());
        };
        // A VM that has already exited (it died during boot) only needs its handle closed.
        if !system.wait_exited(Duration::ZERO) {
            // A guest that cannot take the request (still booting, or hung) is terminated
            // below.
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
        }
        self.system = None;
        *self.vm_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // The VM is gone, so its end of the console pipe closes and the rest is read.
        self.end_console(CONSOLE_DRAIN);
        Ok(())
    }

    fn exited(&mut self) -> Option<String> {
        let system = self.system.as_ref()?;
        system
            .wait_exited(Duration::ZERO)
            .then(|| "the Android VM exited during boot".to_string())
    }

    fn diagnostics(&mut self) -> Option<String> {
        self.console_report
            .clone()
            .or_else(|| self.console.as_ref().map(ConsoleCapture::failure_report))
    }

    fn vm_id(&self) -> Option<String> {
        self.runtime_id().ok().map(|g| g.to_string())
    }
}
