//! The Android VM as the session's guest launcher on macOS.
//!
//! Each start makes a new VM from the configuration source, as the Windows host makes a new
//! compute system: the image may have changed since the last boot. The boot's secret reaches
//! the guest as a bootconfig appended to a copy of the image's initrd (with the user's device
//! identity, when they changed it, as a property file before it). The copy is readable only
//! by the user, and is deleted once the VM has started and read it.
//!
//! Stopping asks the guest to shut down and stops the VM outright if it has not within the
//! stop timeout. Memory grows and shrinks through the balloon; freezing pauses the VM.
//!
//! The guest's kernel runs with `panic=-1`, and the Virtualization framework resets a VM
//! that reboots in place: the VM never ends, so the session would see only its control
//! channel close while an orphaned boot ran on with no host. The launcher reads the console
//! as it grows ([`ConsoleWatch`]); a kernel panic or a new boot on it counts as the guest
//! ending, and the VM is stopped at once.

use dsa_host_core::guest_crash::ConsoleWatch;
use dsa_host_core::start_failure::FailureSide;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::boot_signs::{
    explain_unfinished_boot, explain_unreachable_agent, BootSigns, BootWords,
};
use dsa_host_core::bootconfig;
use dsa_host_core::device_identity::{append_overlay, ramdisk_overlay};
use dsa_host_core::memory::growth_failed_line;
use dsa_host_core::session::{GuestLauncher, GuestStall};
use dsa_protocol::auth::{bootconfig_entry, BootSecret};

use crate::host_memory::{vm_processes, VmProcess};
use crate::vm_config::MacVmConfig;
use crate::vz::{PortChannels, VirtualMachine};

/// What one boot is made of.
pub struct BootPlan {
    /// The VM, with the image's own initrd; the launcher swaps in the boot's copy.
    pub vm: MacVmConfig,
    /// Android's ramdisk properties for this boot (the user's device identity), if any.
    pub properties: Option<String>,
    /// More bootconfig lines (`key = "value"`), beside the secret.
    pub bootconfig: String,
}

/// Supplies each boot's plan.
pub type PlanSource = Box<dyn FnMut() -> Result<BootPlan, String> + Send>;

/// Says what the VM cannot run without and is missing now.
pub type Prerequisite = Box<dyn FnMut() -> Option<String> + Send>;

/// The running VM, shared with what dials the guest's ports.
#[derive(Clone, Default)]
pub struct VmSlot(Arc<Mutex<Option<Arc<VirtualMachine>>>>);

impl VmSlot {
    pub fn get(&self) -> Option<Arc<VirtualMachine>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn set(&self, vm: Option<Arc<VirtualMachine>>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = vm;
    }
    /// Connects to the guest's vsock port `port`.
    pub fn connect(&self, port: u32, timeout: Duration) -> std::io::Result<std::os::fd::OwnedFd> {
        match self.get() {
            Some(vm) => vm.connect(port, timeout),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "Android is not running",
            )),
        }
    }
}

/// A file removed when this is dropped.
struct TransientFile(PathBuf);

impl Drop for TransientFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// What this host tells the user about a late boot that no other host would.
const BOOT_WORDS: BootWords<'static> = BootWords {
    renderer_note:
        "The renderer (dsa-gfx) is started by DSA's window manager (dsa-wm), which must \
                    run in your login session; its log is in DSA's logs folder.",
    start_over: "run `dsa stop` and move",
};

/// How many lines of the console a failed start reports.
const CONSOLE_LINES: usize = 30;

pub struct MacVmLauncher {
    source: PlanSource,
    channels: Arc<PortChannels>,
    slot: VmSlot,
    boot_dir: PathBuf,
    console_log: Option<PathBuf>,
    /// What Android's log showed while it booted, which whoever reads that log fills in. Each
    /// boot starts it afresh.
    log_signs: Arc<Mutex<BootSigns>>,
    /// The writable disk of the last boot, which is Android's data.
    data_disk: Option<PathBuf>,
    prerequisite: Option<Prerequisite>,
    /// The memory the running VM started with and may grow to, in MiB.
    memory: Option<(u64, u64)>,
    boots: u64,
    /// The framework's process that runs the VM, for the memory report.
    process: VmProcess,
    /// What this boot's console has shown of a kernel crash or a reset in place.
    watch: ConsoleWatch,
    /// How much of this boot's console the watch has read, in bytes.
    console_read: u64,
}

impl MacVmLauncher {
    pub fn new(
        source: PlanSource,
        channels: Arc<PortChannels>,
        boot_dir: PathBuf,
        console_log: Option<PathBuf>,
    ) -> MacVmLauncher {
        MacVmLauncher {
            source,
            channels,
            slot: VmSlot::default(),
            boot_dir,
            console_log,
            log_signs: Arc::default(),
            data_disk: None,
            prerequisite: None,
            memory: None,
            boots: 0,
            process: VmProcess::default(),
            watch: ConsoleWatch::new(),
            console_read: 0,
        }
    }

    pub fn with_prerequisite(mut self, prerequisite: Prerequisite) -> MacVmLauncher {
        self.prerequisite = Some(prerequisite);
        self
    }

    /// Reads the signs of a late boot from `signs` as well as from the console: whoever reads
    /// Android's log while it boots fills it in.
    pub fn with_log_signs(mut self, signs: Arc<Mutex<BootSigns>>) -> MacVmLauncher {
        self.log_signs = signs;
        self
    }

    /// What this boot's console and Android's log have shown about why it is late.
    fn signs(&self) -> BootSigns {
        let mut signs = self
            .console_log
            .as_ref()
            .and_then(|log| std::fs::read(log).ok())
            .map(|bytes| BootSigns::from_console(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_default();
        signs.merge(&self.log_signs.lock().unwrap_or_else(|e| e.into_inner()));
        signs
    }

    /// Records the running VM's process in `process`.
    pub fn with_process(mut self, process: VmProcess) -> MacVmLauncher {
        self.process = process;
        self
    }

    /// The running VM, for dialing the guest's ports.
    pub fn slot(&self) -> VmSlot {
        self.slot.clone()
    }

    /// Writes this boot's initrd: the image's, the properties, then the secret and `more` as
    /// bootconfig. Readable only by this user.
    fn write_initrd(
        &self,
        image_initrd: &std::path::Path,
        properties: Option<&str>,
        secret: &BootSecret,
        more: &str,
    ) -> Result<TransientFile, String> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut image = std::fs::read(image_initrd)
            .map_err(|e| format!("could not read the initrd {}: {e}", image_initrd.display()))?;
        if let Some(properties) = properties {
            image = append_overlay(&image, &ramdisk_overlay(properties));
        }
        std::fs::create_dir_all(&self.boot_dir)
            .map_err(|e| format!("could not create {}: {e}", self.boot_dir.display()))?;
        let path = self
            .boot_dir
            .join(format!("boot-{}-{}.initrd", std::process::id(), self.boots));
        let file = TransientFile(path.clone());
        let text = format!("{}{more}", bootconfig_entry(secret));
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("could not write the boot's initrd {}: {e}", path.display()))?;
        std::io::Write::write_all(&mut out, &bootconfig::append(&image, &text))
            .map_err(|e| format!("could not write the boot's initrd {}: {e}", path.display()))?;
        Ok(file)
    }

    /// The last lines of the console log, and where the rest is.
    fn console_tail(&self) -> Option<String> {
        let log = self.console_log.as_ref()?;
        let text = std::fs::read_to_string(log).ok()?;
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(CONSOLE_LINES)..].join("\n");
        if tail.trim().is_empty() {
            return Some(format!(
                "the guest printed nothing on its console ({})",
                log.display()
            ));
        }
        Some(format!(
            "the guest's console ended with:\n{tail}\n(the whole console is in {})",
            log.display()
        ))
    }

    /// Reads what the console printed since the last look into the watch, and returns what
    /// it showed of a crash, the first time.
    fn read_console(&mut self) -> Option<dsa_host_core::guest_crash::ConsoleEvent> {
        let log = self.console_log.as_ref()?;
        let mut file = std::fs::File::open(log).ok()?;
        file.seek(SeekFrom::Start(self.console_read)).ok()?;
        let mut bytes = Vec::new();
        // At most a few MiB a look, so a flood of output never holds the session up.
        file.take(4 << 20).read_to_end(&mut bytes).ok()?;
        // A multi-byte character cut at the end is read whole next time.
        let whole = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => bytes.len(),
        };
        self.console_read += whole as u64;
        self.watch.feed(&String::from_utf8_lossy(&bytes[..whole]))
    }

    /// Keeps the previous boot's console as `<name>.1.log`, so each boot's log starts empty.
    fn rotate_console(&self) {
        if let Some(log) = &self.console_log {
            if log.exists() {
                let _ = std::fs::rename(log, log.with_extension("1.log"));
            }
        }
    }
}

impl GuestLauncher for MacVmLauncher {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        if self.slot.get().is_some() {
            return Err("Android's VM is already running".into());
        }
        if !crate::vz::supported() {
            return Err(
                "this Mac cannot run virtual machines: Apple's Virtualization framework \
                 reports no support (a Mac that is itself a VM needs nested virtualization)"
                    .into(),
            );
        }
        let mut plan = (self.source)()?;
        self.boots += 1;
        let initrd = self.write_initrd(
            &plan.vm.initrd,
            plan.properties.as_deref(),
            secret,
            &plan.bootconfig,
        )?;
        plan.vm.initrd = initrd.0.clone();
        plan.vm.console_log = self.console_log.clone();
        self.rotate_console();
        self.watch = ConsoleWatch::new();
        self.console_read = 0;
        *self.log_signs.lock().unwrap_or_else(|e| e.into_inner()) = BootSigns::default();
        self.data_disk = plan
            .vm
            .disks
            .iter()
            .find(|disk| !disk.read_only)
            .map(|disk| disk.path.clone());
        self.channels.drain();
        let before = vm_processes();
        let vm = Arc::new(VirtualMachine::create(&plan.vm, &self.channels)?);
        vm.start()?;
        self.process
            .set(vm_processes().into_iter().find(|pid| !before.contains(pid)));
        // The guest has read the initrd by the time it runs.
        drop(initrd);
        if plan.vm.memory_start_mb < plan.vm.memory_max_mb {
            if let Err(why) = vm.set_memory_mb(plan.vm.memory_start_mb) {
                eprintln!("memory: the balloon cannot hold the VM at its start size: {why}");
            }
        }
        self.memory = Some((plan.vm.memory_start_mb, plan.vm.memory_max_mb));
        self.slot.set(Some(vm));
        Ok(())
    }

    fn stop(&mut self, timeout: Duration) -> Result<(), String> {
        let Some(vm) = self.slot.get() else {
            return Ok(());
        };
        if vm.ended().is_none() {
            // A paused VM cannot shut down by itself.
            let _ = vm.resume();
            if vm.request_stop().is_ok() {
                let deadline = Instant::now() + timeout;
                while vm.ended().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        if vm.ended().is_none() {
            vm.force_stop()?;
        }
        if vm.ended().is_none() {
            return Err("Android's VM did not stop".into());
        }
        self.slot.set(None);
        self.process.set(None);
        self.memory = None;
        Ok(())
    }

    fn exited(&mut self) -> Option<String> {
        let vm = self.slot.get()?;
        if let Some(event) = self.watch.event() {
            return Some(event.sentence().to_string());
        }
        if let Some(ended) = vm.ended() {
            // A panic printed just before the end is read for the report.
            let _ = self.read_console();
            return Some(format!("Android's VM ended by itself ({ended})"));
        }
        let event = self.read_console()?;
        // The boot the VM reset into has no host: it is stopped before it gets further.
        eprintln!(
            "lifecycle: {}; stopping the VM that restarted",
            event.sentence()
        );
        if let Err(why) = vm.force_stop() {
            eprintln!("lifecycle: the VM that restarted by itself did not stop: {why}");
        }
        Some(event.sentence().to_string())
    }

    fn crash_console(&mut self) -> Option<String> {
        self.watch.report()
    }

    /// A start that returned an error never ran the guest: the Virtualization framework
    /// refused to create or start the VM. A VM that ended during boot failed on the host when
    /// the guest printed nothing on its console.
    fn failure_side(&mut self) -> FailureSide {
        if self.slot.get().is_none() {
            return FailureSide::Host;
        }
        FailureSide::from_guest_output(
            self.console_log
                .as_ref()
                .is_none_or(|log| std::fs::metadata(log).is_ok_and(|meta| meta.len() > 0)),
        )
    }

    fn diagnostics(&mut self) -> Option<String> {
        self.console_tail()
    }

    fn vm_id(&self) -> Option<String> {
        self.slot
            .get()
            .map(|_| format!("vz-{}-{}", std::process::id(), self.boots))
    }

    fn prerequisite(&mut self) -> Option<String> {
        self.prerequisite.as_mut().and_then(|check| check())
    }

    fn explain_stall(&mut self, stall: GuestStall) -> Option<String> {
        let log = self.console_log.clone()?;
        let signs = self.signs();
        Some(match stall {
            GuestStall::BootUnfinished => explain_unfinished_boot(&signs, &log, &BOOT_WORDS),
            GuestStall::AgentUnreachable => {
                explain_unreachable_agent(&signs, &log, self.data_disk.as_deref(), &BOOT_WORDS)
            }
        })
    }

    fn agent_refused_for_keys(&mut self) -> bool {
        self.signs().agent_refused_for_keys()
    }

    fn freeze(&mut self) -> Result<(), String> {
        match self.slot.get() {
            Some(vm) => vm.pause(),
            None => Ok(()),
        }
    }

    fn thaw(&mut self) -> Result<(), String> {
        match self.slot.get() {
            Some(vm) => vm.resume(),
            None => Ok(()),
        }
    }

    fn memory_range_mb(&self) -> Option<(u64, u64)> {
        self.memory.filter(|(start, max)| start < max)
    }

    fn set_memory_mb(&mut self, mb: u64) -> Result<(), String> {
        match self.slot.get() {
            Some(vm) => vm.set_memory_mb(mb),
            None => Err("Android is not running".into()),
        }
    }

    fn memory_growth_failed(&mut self, why: &str) {
        eprintln!(
            "{}",
            growth_failed_line(why, self.memory.map(|(start, _)| start))
        );
    }
}
