//! The Android VM as the session's guest launcher.
//!
//! With a console log, each boot's serial console is kept in that file (see
//! `console_pipe`), the session hears at once when the VM exits during boot, and a start
//! that fails reports the console's last lines.
//!
//! Each boot's secret reaches the guest as a bootconfig appended to a copy of the image's
//! initrd. The copy exists only while the VM starts: the VM has read it by the time it runs,
//! and it is deleted then. The user's device identity, when they changed it, travels in the
//! same copy, as a property file in an archive between the image's initrd and the bootconfig.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use dsa_host_core::bootconfig;
use dsa_host_core::device_identity::{append_overlay, ramdisk_overlay};
use dsa_host_core::session::{GuestLauncher, GuestStall};
use dsa_protocol::auth::{bootconfig_entry, BootSecret};

use crate::console_log::{
    boot_header, explain_unfinished_boot, explain_unreachable_agent, ConsoleLog, ConsoleSigns,
};
use crate::console_pipe::ConsoleCapture;
use crate::guid::Guid;
use crate::hcs::{grant_vm_access, ComputeSystem};
use crate::vm::{
    fine_grained_memory_supported, hosting_process_name_suffix_supported, AndroidVmConfig,
    GuestArch, HOSTING_PROCESS_NAME_SUFFIX,
};
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
    /// Says what the VM cannot run without and is missing, and tries to provide it.
    prerequisite: Option<Prerequisite>,
    /// The disk the running VM boots from, for what a stalled boot tells the user to do.
    data_disk: Option<PathBuf>,
    /// What Android's log has shown during this boot, which the console no longer carries.
    log_signs: Arc<Mutex<ConsoleSigns>>,
    /// How far the running VM may grow, in MiB, as the configuration source last set it;
    /// `None` keeps it at the size it started with.
    memory_max: MemoryMax,
    /// The size the running VM started with, in MiB.
    memory_base: Option<u64>,
    /// More bootconfig for each boot, beside the secret.
    bootconfig: Option<BootconfigSource>,
    /// The properties each boot hands Android in its initrd, if any.
    boot_properties: Option<BootProperties>,
    /// The VM's network, made at each start and removed at each stop. Without it the VM has
    /// no network adapter.
    networking: Option<crate::hcn::VmNetworking>,
}

/// Supplies more bootconfig lines (`key = "value"`) for each boot of the VM configured so.
/// See [`AndroidVmLauncher::with_bootconfig`].
pub type BootconfigSource = Box<dyn FnMut(&AndroidVmConfig) -> String + Send>;

/// See [`AndroidVmLauncher::with_boot_properties`].
pub type BootProperties = Box<dyn FnMut(&AndroidVmConfig) -> Option<String> + Send>;

/// See [`AndroidVmLauncher::with_prerequisite`].
pub type Prerequisite = Box<dyn FnMut() -> Option<String> + Send>;

/// Where the configuration source says how far the next VM may grow, in MiB. See
/// [`AndroidVmLauncher::with_memory_max`].
pub type MemoryMax = Arc<Mutex<Option<u64>>>;

/// A file that is removed when this is dropped.
pub struct TransientFile(pub PathBuf);

impl Drop for TransientFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Writes `initrd`, then `properties` as Android's ramdisk property file when there are any,
/// with `secret` and `more` appended as bootconfig, to `path`, readable only by the account
/// the host runs as until the VM is let in.
pub fn write_boot_initrd(
    initrd: &Path,
    properties: Option<&str>,
    secret: &BootSecret,
    more: &str,
    path: &Path,
) -> Result<TransientFile, String> {
    let mut image = std::fs::read(initrd)
        .map_err(|e| format!("could not read the initrd {}: {e}", initrd.display()))?;
    if let Some(properties) = properties {
        image = append_overlay(&image, &ramdisk_overlay(properties));
    }
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("could not create {}: {e}", folder.display()))?;
    }
    let file = TransientFile(path.to_path_buf());
    let text = format!("{}{more}", bootconfig_entry(secret));
    std::fs::write(path, bootconfig::append(&image, &text))
        .map_err(|e| format!("could not write the boot's initrd {}: {e}", path.display()))?;
    Ok(file)
}

/// A folder that is removed, with what is in it, when this is dropped.
struct TransientDir(PathBuf);

impl Drop for TransientDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One boot's files: the initrd with the boot's secret and, on ARM64, the folder the VM's
/// firmware boots from. Everything is removed when this is dropped.
pub struct BootFiles {
    // Dropped in this order: the initrd, then the folder that may hold it.
    initrd: TransientFile,
    folder: Option<TransientDir>,
    kernel_copy: Option<PathBuf>,
}

impl BootFiles {
    /// The boot's initrd.
    pub fn initrd(&self) -> &Path {
        &self.initrd.0
    }

    /// What the VM must be let in to before it starts.
    pub fn vm_access(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(folder) = &self.folder {
            paths.push(folder.0.clone());
        }
        paths.extend(self.kernel_copy.clone());
        paths.push(self.initrd.0.clone());
        paths
    }
}

/// Writes the files of one boot of `config` into `boot_dir` and points `config` at them.
///
/// On x86_64 that is `<unique>.initrd`, the image's initrd with `secret` appended. On ARM64
/// the VM's firmware reads the kernel and the initrd from one folder, and the VM can read all
/// of that folder, so each boot gets a folder of its own, `<unique>`, holding a copy of the
/// kernel and `initrd.img`: no other boot's secret is ever in it.
pub fn prepare_boot(
    arch: GuestArch,
    config: &mut AndroidVmConfig,
    properties: Option<&str>,
    secret: &BootSecret,
    more: &str,
    boot_dir: &Path,
    unique: &str,
) -> Result<BootFiles, String> {
    let Some(initrd) = config.initrd.clone() else {
        return Err(
            "the Android VM has no initrd, which is what carries the boot's secret to \
             the guest"
                .into(),
        );
    };
    let mut folder = None;
    let mut kernel_copy = None;
    let initrd_path = if arch.boots_from_a_folder() {
        let dir = boot_dir.join(unique);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        folder = Some(TransientDir(dir.clone()));
        let name = config
            .kernel
            .file_name()
            .ok_or_else(|| format!("the kernel {} is not a file", config.kernel.display()))?;
        let copy = dir.join(name);
        std::fs::copy(&config.kernel, &copy).map_err(|e| {
            format!(
                "could not copy the kernel {} to {}: {e}",
                config.kernel.display(),
                copy.display()
            )
        })?;
        config.kernel = copy.clone();
        kernel_copy = Some(copy);
        dir.join("initrd.img")
    } else {
        boot_dir.join(format!("{unique}.initrd"))
    };
    let written = write_boot_initrd(&initrd, properties, secret, more, &initrd_path)?;
    config.initrd = Some(initrd_path);
    Ok(BootFiles {
        initrd: written,
        folder,
        kernel_copy,
    })
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
            boot_dir: crate::paths::MachineRoot::default_location().tmp(),
            prerequisite: None,
            data_disk: None,
            log_signs: Arc::new(Mutex::new(ConsoleSigns::default())),
            memory_max: MemoryMax::default(),
            memory_base: None,
            bootconfig: None,
            boot_properties: None,
            networking: None,
        }
    }

    /// Gives each boot a network adapter on the network `networking` makes for it.
    pub fn with_networking(mut self, networking: crate::hcn::VmNetworking) -> AndroidVmLauncher {
        self.networking = Some(networking);
        self
    }

    /// Lets the running VM grow to the size in `max`, which the configuration source sets
    /// before each start: it starts at its configured memory and memory is hot-added while the
    /// guest is short of it.
    pub fn with_memory_max(mut self, max: MemoryMax) -> AndroidVmLauncher {
        self.memory_max = max;
        self
    }

    /// Asks `source` at each start for more bootconfig lines, beside the boot's secret: what
    /// the guest is to know before its init reads any property, such as which ARM translation
    /// package to use. It is given the VM's configuration, whose files say which image boots.
    pub fn with_bootconfig(mut self, source: BootconfigSource) -> AndroidVmLauncher {
        self.bootconfig = Some(source);
        self
    }

    /// Asks `properties` at every start for the property file to hand Android in the boot's
    /// initrd: the user's device identity, which Android reads once while it boots.
    pub fn with_boot_properties(mut self, properties: BootProperties) -> AndroidVmLauncher {
        self.boot_properties = Some(properties);
        self
    }

    /// Reads the signs of a late boot from `signs` as well as from the console: whoever reads
    /// Android's log while it boots fills it in. Each boot starts it afresh.
    pub fn with_log_signs(mut self, signs: Arc<Mutex<ConsoleSigns>>) -> AndroidVmLauncher {
        self.log_signs = signs;
        self
    }

    /// What this boot's console and Android's log have shown about why it is late.
    fn signs(&self) -> ConsoleSigns {
        let mut signs = self
            .console
            .as_ref()
            .map(ConsoleCapture::signs)
            .unwrap_or_default();
        signs.merge(&self.log_signs.lock().unwrap_or_else(|e| e.into_inner()));
        signs
    }

    /// Has the session ask `prerequisite` for what the VM cannot run without: it tries to
    /// provide that, and says what is missing, if anything. The VM does not start without it.
    pub fn with_prerequisite(mut self, prerequisite: Prerequisite) -> AndroidVmLauncher {
        self.prerequisite = Some(prerequisite);
        self
    }

    /// Writes each boot's initrd into `dir` instead of the machine's `tmp` folder in
    /// `%ProgramData%\DSA`.
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

    /// Creates and starts the VM `config` describes, booting from `boot`.
    fn create_and_start(
        &mut self,
        config: &AndroidVmConfig,
        boot: &BootFiles,
    ) -> Result<(), String> {
        let document = config
            .to_hcs_document()
            .map_err(|e| e.to_string())?
            .to_string();
        let system =
            ComputeSystem::create(&self.id, &document, CALL_TIMEOUT).map_err(|e| e.to_string())?;
        // The VM opens its disk when it starts. A disk in the user's profile admits only the
        // user, SYSTEM and administrators, so the VM is let in to that one file first.
        let mut access = vec![config.data_disk.clone()];
        access.extend(boot.vm_access());
        access.extend(config.pstore_disk.iter().cloned());
        for path in &access {
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
        self.memory_base = Some(u64::from(config.memory_mb));
        Ok(())
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
        *self.log_signs.lock().unwrap_or_else(|e| e.into_inner()) = ConsoleSigns::default();
        let mut config = (self.config)()?;
        config.check_settings().map_err(|e| e.to_string())?;
        self.data_disk = Some(config.data_disk.clone());
        // Removed on every way out of this function: by then the VM has read them or will
        // never run.
        let more = self
            .bootconfig
            .as_mut()
            .map(|source| source(&config))
            .unwrap_or_default();
        let properties = self
            .boot_properties
            .as_mut()
            .and_then(|properties| properties(&config));
        config.kernel_cmdline = format!("{} {}", config.kernel_cmdline, bootconfig::CMDLINE_FLAG);
        // The console is kept from here, so a boot whose files cannot be prepared is still
        // in the log with its reason.
        config.console_pipe = self.begin_console(&config);
        let boot = prepare_boot(
            GuestArch::HOST,
            &mut config,
            properties.as_deref(),
            secret,
            &more,
            &self.boot_dir,
            &crate::paths::unique_name(&self.id, ""),
        )?;
        if windows_build()
            .is_some_and(|(build, ubr)| hosting_process_name_suffix_supported(build, ubr))
        {
            config.hosting_process_name_suffix = Some(HOSTING_PROCESS_NAME_SUFFIX.into());
        }
        config.fine_grained_memory =
            windows_build().is_some_and(|(build, ubr)| fine_grained_memory_supported(build, ubr));
        // The guest kernel's last log of the boot before is on this disk; one that is missing
        // or damaged is made again, empty, and Android starts either way.
        if let Some(pstore) = &config.pstore_disk {
            if let Err(why) = crate::pstore_disk::ensure(pstore) {
                eprintln!("{why}; Android starts without its last log");
                config.pstore_disk = None;
            }
        }
        if let Some(networking) = &mut self.networking {
            config.network_adapter = networking.attach(&self.id);
        }
        let started = self.create_and_start(&config, &boot);
        // A VM that did not come up leaves no network behind, whether or not the session
        // stops it afterwards.
        if started.is_err() {
            if let Some(networking) = &mut self.networking {
                networking.detach();
            }
        }
        started
    }

    fn stop(&mut self, timeout: Duration) -> Result<(), String> {
        let Some(system) = self.system.as_ref() else {
            // A start that failed part of the way may have served the console already, and
            // made the network.
            self.end_console(CONSOLE_DRAIN);
            if let Some(networking) = &mut self.networking {
                networking.detach();
            }
            return Ok(());
        };
        // A VM that has already exited (it died during boot, or crashed) only needs its handle
        // closed.
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
        self.memory_base = None;
        *self.vm_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // The VM is gone, so its end of the console pipe closes and the rest is read.
        self.end_console(CONSOLE_DRAIN);
        // Its network goes with it.
        if let Some(networking) = &mut self.networking {
            networking.detach();
        }
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

    fn prerequisite(&mut self) -> Option<String> {
        // Asked about once a second while the VM runs: the time to keep an advanced network
        // on the adapter the host uses now.
        if let (Some(networking), Some(system)) = (&mut self.networking, &self.system) {
            networking.follow(&self.id, &|request: &str| {
                system
                    .modify(request, CALL_TIMEOUT)
                    .map_err(|e| e.to_string())
            });
        }
        self.prerequisite.as_mut().and_then(|check| check())
    }

    fn agent_refused_for_keys(&mut self) -> bool {
        self.signs().agent_refused_for_keys()
    }

    fn explain_stall(&mut self, stall: GuestStall) -> Option<String> {
        let log = self.console.as_ref()?.log_path();
        let signs = self.signs();
        Some(match stall {
            GuestStall::BootUnfinished => explain_unfinished_boot(&signs, &log),
            GuestStall::AgentUnreachable => {
                explain_unreachable_agent(&signs, &log, self.data_disk.as_deref())
            }
        })
    }

    /// Pauses the whole VM. Its memory stays allocated to the VM process, where Windows can
    /// page it out like any process's, and nothing in the guest runs until the thaw.
    fn freeze(&mut self) -> Result<(), String> {
        let system = self
            .system
            .as_ref()
            .ok_or("the Android VM is not running")?;
        system
            .pause(CALL_TIMEOUT)
            .map_err(|e| format!("the Android VM could not be paused: {e}"))
    }

    fn thaw(&mut self) -> Result<(), String> {
        let Some(system) = self.system.as_ref() else {
            return Ok(());
        };
        system
            .resume(CALL_TIMEOUT)
            .map_err(|e| format!("the Android VM could not be resumed: {e}"))
    }

    fn memory_range_mb(&self) -> Option<(u64, u64)> {
        self.system.as_ref()?;
        let base = self.memory_base?;
        let max = (*self.memory_max.lock().unwrap_or_else(|e| e.into_inner()))?;
        (max > base).then_some((base, max))
    }

    fn set_memory_mb(&mut self, mb: u64) -> Result<(), String> {
        let system = self
            .system
            .as_ref()
            .ok_or("the Android VM is not running")?;
        system
            .modify(&crate::vm::memory_resize_request(mb), CALL_TIMEOUT)
            .map_err(|e| format!("the Android VM's memory could not be set to {mb} MiB: {e}"))
    }

    fn memory_growth_failed(&mut self, why: &str) {
        // The guest keeps the memory it started with. Whether to start it larger instead is
        // a decision about idle memory, so it is reported rather than made here.
        eprintln!(
            "memory: the Android VM cannot grow on this computer ({why}); it keeps the {} MiB \
             it started with",
            self.memory_base.unwrap_or_default()
        );
    }
}

/// This Windows build's number and update revision, as the registry records them.
pub(crate) fn windows_build() -> Option<(u32, u32)> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let key = wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let build_name = wide("CurrentBuildNumber");
    let ubr_name = wide("UBR");
    let mut build = [0u16; 16];
    let mut size = std::mem::size_of_val(&build) as u32;
    // SAFETY: the buffer and its size in bytes describe the same memory.
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            build_name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            build.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if code != ERROR_SUCCESS {
        return None;
    }
    let len = build.iter().position(|&c| c == 0).unwrap_or(build.len());
    let build: u32 = String::from_utf16_lossy(&build[..len])
        .trim()
        .parse()
        .ok()?;
    let mut ubr = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: as above, for a DWORD. A build without UBR reads as revision 0.
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            ubr_name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut ubr as *mut u32).cast(),
            &mut size,
        )
    };
    if code != ERROR_SUCCESS {
        ubr = 0;
    }
    Some((build, ubr))
}
