//! The Android VM as the session's guest launcher: crosvm under KVM, run as the user it is for.
//!
//! Each boot's secret reaches the guest as a bootconfig appended to a copy of the image's
//! initrd, readable only by the user, as on every host. The copy exists only while the VM
//! starts: crosvm has loaded it into the guest's memory by the time the guest kernel prints
//! its first line, and it is deleted then. The user's device identity, when they changed it,
//! travels in the same copy.
//!
//! Stopping asks the guest to sync its disks and power off (SysRq on the console, see
//! `crosvm::SHUTDOWN_SEQUENCE`), ends the VM through crosvm's control socket if it has not
//! exited within the stop timeout, and kills crosvm if that does not work either. Every guest
//! process has ended once `stop` returns `Ok`, because the VM itself is gone.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dsa_host_core::bootconfig;
use dsa_host_core::device_identity::{append_overlay, ramdisk_overlay};
use dsa_host_core::host_api::NetworkStatus;
use dsa_host_core::session::{GuestLauncher, GuestStall};
use dsa_protocol::auth::{bootconfig_entry, BootSecret};

use crate::console::{boot_header, capture, Tail};
use crate::crosvm::{virtio_mem_bytes, GuestArch, NetworkBackend, VmConfig, SHUTDOWN_SEQUENCE};
use crate::crosvm_control::{Control, Request};
use crate::image::ImageFiles;
use crate::network::{place_tap, VmNetwork};
use crate::signs::{explain_unfinished_boot, explain_unreachable_agent, BootSigns};
use crate::users::{Account, BeforeExec};

/// How long crosvm may take to open its control socket and start the guest kernel.
const START_WAIT: Duration = Duration::from_secs(30);
/// How long crosvm may take to exit after it is told to stop or is killed.
const QUIT_GRACE: Duration = Duration::from_secs(10);
/// The pause between the steps of the guest's shutdown, for each to finish.
const SYSRQ_STEP: Duration = Duration::from_millis(1500);

/// What one boot starts: the image's files and version, and its memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootPlan {
    pub files: ImageFiles,
    pub image_version: Option<String>,
    /// The memory the VM starts with, in MiB.
    pub memory_mb: u32,
    /// How far it may grow while it runs, in MiB.
    pub memory_max_mb: Option<u32>,
}

/// Supplies the plan of each boot.
pub type PlanSource = Box<dyn FnMut() -> Result<BootPlan, String> + Send>;
/// Supplies more bootconfig lines (`key = "value"`) for each boot of the image whose files it
/// is given, beside the secret.
pub type BootconfigSource = Box<dyn FnMut(&ImageFiles) -> String + Send>;
/// Supplies the properties each boot hands Android in its initrd, if any.
pub type BootProperties = Box<dyn FnMut(&ImageFiles) -> Option<String> + Send>;
/// Says what the VM cannot run without and is missing, and tries to provide it.
pub type Prerequisite = Box<dyn FnMut() -> Option<String> + Send>;
/// Whether the next boot's network is on the LAN (advanced networking) rather than NAT.
pub type AdvancedNetworking = Box<dyn FnMut() -> bool + Send>;
/// The network of a user's running VM, as the local API reports it.
pub type SharedNetworkStatus = Arc<Mutex<Option<NetworkStatus>>>;

/// The crosvm process of each running VM, by the VM's vsock address, for the memory report.
fn running_vms() -> &'static Mutex<HashMap<u32, u32>> {
    static VMS: OnceLock<Mutex<HashMap<u32, u32>>> = OnceLock::new();
    VMS.get_or_init(Default::default)
}

/// The process ID of the crosvm that runs the VM at `cid`.
pub fn vm_process(cid: u32) -> Option<u32> {
    running_vms()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&cid)
        .copied()
}

/// A file that is removed when this is dropped.
struct TransientFile(PathBuf);

impl Drop for TransientFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Networking {
    advanced: AdvancedNetworking,
    network: VmNetwork,
    status: SharedNetworkStatus,
}

struct RunningVm {
    child: Child,
    console_input: Option<ChildStdin>,
    control: Control,
    readers: Vec<JoinHandle<()>>,
    /// The memory it started with and may grow to, in MiB, when it may grow. It boots with
    /// the first, and the rest is its virtio-mem region.
    memory: Option<(u64, u64)>,
}

impl RunningVm {
    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.exited() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Asks the guest to sync, remount read only and power off.
    fn ask_guest_to_power_off(&mut self) {
        let Some(mut input) = self.console_input.take() else {
            return;
        };
        for step in SHUTDOWN_SEQUENCE {
            if input.write_all(step).and_then(|_| input.flush()).is_err() {
                break;
            }
            if self.wait_exit(SYSRQ_STEP) {
                break;
            }
        }
        self.console_input = Some(input);
    }
}

/// Starts and stops one user's Android VM.
pub struct LinuxVmLauncher {
    account: Account,
    name: String,
    guest_cid: u32,
    crosvm: PathBuf,
    sandbox: bool,
    arch: GuestArch,
    processors: u32,
    page_reporting_order: u8,
    data_disk: PathBuf,
    pstore_disk: Option<PathBuf>,
    /// The user's add-on disk, which each start readies and attaches while it holds add-ons.
    addon_disk: Option<PathBuf>,
    /// The user's folder for this VM's sockets and each boot's initrd.
    run_dir: PathBuf,
    console_log: PathBuf,
    plan: PlanSource,
    bootconfig: Option<BootconfigSource>,
    boot_properties: Option<BootProperties>,
    prerequisite: Option<Prerequisite>,
    networking: Option<Networking>,
    /// Whether the VM may grow; turned off for good when growing did not work.
    grow: bool,
    vm: Option<RunningVm>,
    /// The guest's console.
    tail: Tail,
    /// crosvm's own messages.
    vmm_tail: Tail,
    signs: Arc<Mutex<BootSigns>>,
    /// What the last finished boot's console says, for a start that failed.
    report: Option<String>,
    /// Whether each boot maps the read-only disks as virtio-pmem (the host's direct-mapped
    /// mode), where the image can.
    direct_mapped: bool,
}

/// What a launcher is made from: who it is for and where their files are.
pub struct LauncherSetup {
    pub account: Account,
    pub name: String,
    pub guest_cid: u32,
    /// The crosvm program.
    pub crosvm: PathBuf,
    /// Whether crosvm runs each device in its sandbox.
    pub sandbox: bool,
    pub processors: u32,
    pub page_reporting_order: u8,
    pub data_disk: PathBuf,
    pub pstore_disk: Option<PathBuf>,
    /// The user's add-on disk (raw), attached read only while it holds add-ons.
    pub addon_disk: Option<PathBuf>,
    pub run_dir: PathBuf,
    pub console_log: PathBuf,
    pub grow_memory: bool,
}

impl LinuxVmLauncher {
    pub fn new(setup: LauncherSetup, plan: PlanSource) -> LinuxVmLauncher {
        LinuxVmLauncher {
            account: setup.account,
            name: setup.name,
            guest_cid: setup.guest_cid,
            crosvm: setup.crosvm,
            sandbox: setup.sandbox,
            arch: GuestArch::HOST,
            processors: setup.processors,
            page_reporting_order: setup.page_reporting_order,
            data_disk: setup.data_disk,
            pstore_disk: setup.pstore_disk,
            addon_disk: setup.addon_disk,
            run_dir: setup.run_dir,
            console_log: setup.console_log,
            plan,
            bootconfig: None,
            boot_properties: None,
            prerequisite: None,
            networking: None,
            grow: setup.grow_memory,
            vm: None,
            tail: Tail::default(),
            vmm_tail: Tail::default(),
            signs: Arc::default(),
            report: None,
            direct_mapped: false,
        }
    }

    /// Maps the read-only disks (the system disk, and the add-on disk while there is one) as
    /// virtio-pmem at each boot whose image carries the direct-mapped mode's mount table, so
    /// the guest maps their files instead of caching them (`dsa_host_core::direct_mapped`).
    /// Off, every boot attaches them as virtio-blk disks.
    pub fn with_direct_mapped(mut self, on: bool) -> LinuxVmLauncher {
        self.direct_mapped = on;
        self
    }

    /// How this boot maps its read-only disks, when the direct-mapped mode is on and the image
    /// `files` names can: the image's support for it, and whether the add-on disk at
    /// `addon_disk` is in the layout the mode maps (it is written again in it if not).
    fn direct_mapped_disks(
        &self,
        files: &ImageFiles,
        addon_disk: Option<&Path>,
    ) -> Option<crate::crosvm::DirectMapped> {
        if !self.direct_mapped {
            return None;
        }
        let why = |why: &str| {
            eprintln!(
                "{}: {why}; Android starts with its disks on virtio-blk this time",
                self.name
            )
        };
        if files.system_image.is_none() {
            why("the image has no separate system disk to map");
            return None;
        }
        let Some(support) = files
            .initrd
            .as_deref()
            .and_then(Path::parent)
            .and_then(dsa_host_core::direct_mapped::ImageSupport::from_image_dir)
        else {
            why("the image has no mount table for the direct-mapped mode");
            return None;
        };
        let addons = addon_disk.is_some_and(|path| {
            let disk = dsa_host_core::addon_disk::AddonDisk::new(
                path,
                dsa_host_core::addon_disk::Format::Raw,
            );
            if let Err(e) = disk.bring_to_mappable_layout() {
                eprintln!(
                    "{}: direct-mapped: {e}; the add-on disk stays on virtio-blk",
                    self.name
                );
            }
            disk.is_mappable()
        });
        Some(crate::crosvm::DirectMapped { support, addons })
    }

    /// Gives each boot a network adapter: on the LAN while `advanced` says so, else NAT. The
    /// network's state goes to `status` while the VM runs.
    pub fn with_networking(
        mut self,
        advanced: AdvancedNetworking,
        status: SharedNetworkStatus,
    ) -> LinuxVmLauncher {
        self.networking = Some(Networking {
            advanced,
            network: VmNetwork::default(),
            status,
        });
        self
    }

    pub fn with_bootconfig(mut self, source: BootconfigSource) -> LinuxVmLauncher {
        self.bootconfig = Some(source);
        self
    }

    pub fn with_boot_properties(mut self, properties: BootProperties) -> LinuxVmLauncher {
        self.boot_properties = Some(properties);
        self
    }

    pub fn with_prerequisite(mut self, prerequisite: Prerequisite) -> LinuxVmLauncher {
        self.prerequisite = Some(prerequisite);
        self
    }

    /// Reads the signs of a late boot from `signs` as well, which whoever reads Android's log
    /// while it boots fills in. Each boot starts it afresh.
    pub fn with_signs(mut self, signs: Arc<Mutex<BootSigns>>) -> LinuxVmLauncher {
        self.signs = signs;
        self
    }

    /// The initrd of one boot: the image's, the boot's properties, then the bootconfig with the
    /// secret.
    fn boot_initrd(
        &mut self,
        files: &ImageFiles,
        secret: &BootSecret,
        mapped: Option<&crate::crosvm::DirectMapped>,
    ) -> Result<TransientFile, String> {
        let source = files.initrd.as_ref().ok_or(
            "the Android image has no initrd, which is what carries the boot's secret to the guest",
        )?;
        let mut image = std::fs::read(source)
            .map_err(|e| format!("could not read the initrd {}: {e}", source.display()))?;
        if let Some(properties) = self.boot_properties.as_mut().and_then(|p| p(files)) {
            image = append_overlay(&image, &ramdisk_overlay(&properties));
        }
        let mut more = self
            .bootconfig
            .as_mut()
            .map(|b| b(files))
            .unwrap_or_default();
        // In the direct-mapped mode the add-on images come from their own partitions on the
        // persistent memory, while the add-on disk is mapped.
        match mapped {
            Some(mapped) if mapped.addons => {
                more = dsa_host_core::addon_disk::direct_mapped_bootconfig(&more)
            }
            Some(_) => more.push_str(dsa_host_core::addon_disk::DIRECT_MAPPED_BOOTCONFIG),
            None => {}
        }
        let text = format!("{}{more}", bootconfig_entry(secret));
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = self.run_dir.join(format!("boot-{unique}.initrd"));
        self.account
            .write_private(&path, &bootconfig::append(&image, &text))?;
        Ok(TransientFile(path))
    }

    /// What the guest's console and crosvm last showed, saying the console `did` it.
    fn console_report(&self, did: &str) -> Option<String> {
        let tail = self.tail.text();
        let vmm = self.vmm_tail.text();
        let mut report = String::new();
        if !tail.trim().is_empty() {
            report.push_str(&format!("The Android VM's console {did}:\n{tail}\n"));
        }
        if !vmm.trim().is_empty() {
            report.push_str(&format!("crosvm said:\n{vmm}\n"));
        }
        (!report.is_empty()).then(|| {
            format!(
                "{report}The whole boot is in {}.",
                self.console_log.display()
            )
        })
    }

    fn finish(&mut self, mut vm: RunningVm) {
        let _ = vm.child.wait();
        drop(vm.console_input.take());
        for reader in vm.readers.drain(..) {
            let _ = reader.join();
        }
        let _ = std::fs::remove_file(vm.control.socket());
        running_vms()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.guest_cid);
        if let Some(networking) = &mut self.networking {
            networking.network.detach();
            *networking.status.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        self.report = self.console_report("ended with");
    }

    fn start_vm(&mut self, secret: &BootSecret) -> Result<(), String> {
        let plan = (self.plan)()?;
        self.account.make_dir(&self.run_dir)?;
        if let Some(logs) = self.console_log.parent() {
            self.account.make_dir(logs)?;
        }
        if let Some(pstore) = &self.pstore_disk {
            crate::pstore::ensure(pstore)?;
            self.account.own(pstore)?;
        }
        // The add-on disk this start attaches: a change the user made while Android ran takes
        // the place of the disk before now, and a disk without add-ons is not attached. crosvm
        // opens it as the user.
        let addon_disk = match &self.addon_disk {
            Some(path) => {
                let disk = dsa_host_core::addon_disk::AddonDisk::new(
                    path,
                    dsa_host_core::addon_disk::Format::Raw,
                );
                if disk.prepare_for_boot().is_empty() {
                    None
                } else {
                    self.account.own(path)?;
                    Some(path.clone())
                }
            }
            None => None,
        };
        let direct_mapped = self.direct_mapped_disks(&plan.files, addon_disk.as_deref());
        if direct_mapped.is_some() {
            // A disk written again in the mode's layout is a new file, which crosvm opens as
            // the user.
            if let Some(path) = &addon_disk {
                self.account.own(path)?;
            }
        }
        let initrd = self.boot_initrd(&plan.files, secret, direct_mapped.as_ref())?;
        let (backend, mac, tap) = match &mut self.networking {
            Some(networking) => {
                let advanced = (networking.advanced)();
                let choice = networking
                    .network
                    .attach(advanced, &self.account, &self.run_dir)?;
                *networking.status.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(choice.status.clone());
                (choice.backend, choice.mac_address, choice.tap)
            }
            None => (NetworkBackend::None, None, None),
        };
        let control_socket = self.run_dir.join("crosvm.sock");
        let _ = std::fs::remove_file(&control_socket);
        let sandbox_root = if self.sandbox {
            // An empty folder the sandbox makes each device's root.
            let root = self.run_dir.join("sandbox");
            self.account.make_dir(&root)?;
            Some(root)
        } else {
            None
        };
        let memory_max_mb = if self.grow { plan.memory_max_mb } else { None };
        let config = VmConfig {
            name: self.name.clone(),
            arch: self.arch,
            kernel: plan.files.kernel.clone(),
            initrd: initrd.0.clone(),
            kernel_cmdline: plan.files.kernel_cmdline.clone(),
            memory_mb: plan.memory_mb,
            memory_max_mb,
            processors: self.processors,
            system_image: plan.files.system_image.clone(),
            data_disk: self.data_disk.clone(),
            pstore_disk: self.pstore_disk.clone(),
            addon_disk,
            guest_cid: self.guest_cid,
            page_reporting_order: self.page_reporting_order,
            control_socket: control_socket.clone(),
            sandbox_root,
            network: backend,
            mac_address: mac.unwrap_or_else(|| crate::crosvm::mac_address(self.account.uid)),
            direct_mapped,
        };
        let args = config.crosvm_args().map_err(|e| e.to_string())?;
        let mut command = Command::new(&self.crosvm);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let before: Option<BeforeExec> = tap
            .as_ref()
            .map(|tap| Box::new(place_tap(tap)) as BeforeExec);
        self.account.run_as(&mut command, before);
        self.tail = Tail::default();
        self.vmm_tail = Tail::default();
        *self.signs.lock().unwrap_or_else(|e| e.into_inner()) = BootSigns::default();
        self.report = None;
        let mut child = command.spawn().map_err(|e| {
            format!(
                "could not start {} for the Android VM: {e}",
                self.crosvm.display()
            )
        })?;
        // The child has its own copy of the tap.
        drop(tap);
        running_vms()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.guest_cid, child.id());
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let header = boot_header(
            started,
            plan.image_version.as_deref(),
            &config.full_kernel_cmdline(),
        );
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.extend(
                capture(
                    out,
                    Some(self.console_log.clone()),
                    Some(header),
                    self.tail.clone(),
                    Arc::clone(&self.signs),
                )
                .ok(),
            );
        }
        if let Some(err) = child.stderr.take() {
            readers.extend(
                capture(
                    err,
                    Some(self.console_log.clone()),
                    None,
                    self.vmm_tail.clone(),
                    Arc::clone(&self.signs),
                )
                .ok(),
            );
        }
        let console_input = child.stdin.take();
        let mut vm = RunningVm {
            child,
            console_input,
            control: Control::new(self.crosvm.clone(), control_socket.clone()),
            readers,
            memory: match config.memory_layout() {
                (base, Some(region)) => {
                    Some((u64::from(base), u64::from(base) + u64::from(region)))
                }
                (_, None) => None,
            },
        };
        // crosvm has loaded the kernel and the initrd once the guest kernel prints, and it
        // serves its control socket by then.
        let deadline = Instant::now() + START_WAIT;
        let started = loop {
            if control_socket.exists() && !self.tail.text().is_empty() {
                break Ok(());
            }
            if vm.exited() {
                break Err("crosvm exited while it started the Android VM".to_string());
            }
            if Instant::now() >= deadline {
                break Err(format!(
                    "the Android VM did not start within {} s: crosvm opened no control \
                     socket, or the guest kernel printed nothing",
                    START_WAIT.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        drop(initrd);
        match started {
            Ok(()) => {
                self.vm = Some(vm);
                Ok(())
            }
            Err(why) => {
                let _ = vm.child.kill();
                self.finish(vm);
                Err(why)
            }
        }
    }
}

impl GuestLauncher for LinuxVmLauncher {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        if let Some(mut vm) = self.vm.take() {
            // A VM left from an earlier start goes first; it has no secret this session knows.
            let _ = vm.child.kill();
            self.finish(vm);
        }
        let mut started = self.start_vm(secret);
        if started.is_err() && self.grow && self.vmm_tail.contains("guest memory") {
            // A host that cannot map the VM's virtio-mem region: it keeps its starting size.
            self.memory_growth_failed("crosvm could not make the VM's hot-pluggable memory");
            started = self.start_vm(secret);
        }
        if started.is_err() {
            if let Some(networking) = &mut self.networking {
                networking.network.detach();
                *networking.status.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        started
    }

    fn stop(&mut self, timeout: Duration) -> Result<(), String> {
        let Some(mut vm) = self.vm.take() else {
            if let Some(networking) = &mut self.networking {
                networking.network.detach();
            }
            return Ok(());
        };
        if !vm.exited() {
            // A frozen guest cannot take the request.
            let _ = vm.control.send(Request::Resume);
            vm.ask_guest_to_power_off();
            if !vm.wait_exit(timeout) {
                let _ = vm.control.send(Request::Stop);
                if !vm.wait_exit(QUIT_GRACE) {
                    let _ = vm.child.kill();
                    if !vm.wait_exit(QUIT_GRACE) {
                        let pid = vm.child.id();
                        self.vm = Some(vm);
                        return Err(format!(
                            "the Android VM (crosvm, process {pid}) did not exit within {} s of \
                             being killed",
                            QUIT_GRACE.as_secs()
                        ));
                    }
                }
            }
        }
        self.finish(vm);
        Ok(())
    }

    fn exited(&mut self) -> Option<String> {
        let vm = self.vm.as_mut()?;
        let status = vm.child.try_wait().ok().flatten()?;
        Some(format!(
            "the Android VM exited during boot (crosvm {status})"
        ))
    }

    fn diagnostics(&mut self) -> Option<String> {
        self.report
            .clone()
            .or_else(|| self.console_report("last showed"))
    }

    fn vm_id(&self) -> Option<String> {
        self.vm.as_ref().map(|_| self.guest_cid.to_string())
    }

    fn prerequisite(&mut self) -> Option<String> {
        for (device, what) in [
            ("/dev/kvm", "KVM, which runs the Android VM: turn virtualization on in the computer's firmware settings and load the kvm module"),
            ("/dev/vhost-vsock", "vhost-vsock, which connects the Android VM to DSA: load the vhost_vsock module"),
        ] {
            if !Path::new(device).exists() {
                return Some(format!("{device} is missing. DSA needs {what}."));
            }
        }
        if !self.crosvm.is_absolute() || !self.crosvm.exists() {
            return Some(format!(
                "{} is missing. DSA runs Android with the crosvm its package installs: \
                 reinstall DSA (the desktop-subsystem-for-android package).",
                self.crosvm.display()
            ));
        }
        self.prerequisite.as_mut().and_then(|check| check())
    }

    fn explain_stall(&mut self, stall: GuestStall) -> Option<String> {
        let signs = self.signs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        Some(match stall {
            GuestStall::BootUnfinished => explain_unfinished_boot(&signs, &self.console_log),
            GuestStall::AgentUnreachable => {
                explain_unreachable_agent(&signs, &self.console_log, &self.data_disk)
            }
        })
    }

    fn agent_refused_for_keys(&mut self) -> bool {
        self.signs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .agent_refused_for_keys()
    }

    /// Pauses the whole VM. Its memory stays with crosvm, where Linux can reclaim and swap it
    /// like any process's, and nothing in the guest runs until the thaw.
    fn freeze(&mut self) -> Result<(), String> {
        let vm = self.vm.as_ref().ok_or("the Android VM is not running")?;
        vm.control
            .send(Request::Suspend)
            .map_err(|e| format!("the Android VM could not be paused: {e}"))
    }

    fn thaw(&mut self) -> Result<(), String> {
        let Some(vm) = self.vm.as_ref() else {
            return Ok(());
        };
        vm.control
            .send(Request::Resume)
            .map_err(|e| format!("the Android VM could not be resumed: {e}"))
    }

    fn memory_range_mb(&self) -> Option<(u64, u64)> {
        self.vm.as_ref()?.memory
    }

    /// Asks the guest to plug in memory up to `mb` MiB, or to unplug back down to it: the
    /// virtio-mem requested size is what `mb` adds to the memory the VM booted with. The guest
    /// plugs or unplugs blocks to reach it, and crosvm frees each unplugged block at once.
    fn set_memory_mb(&mut self, mb: u64) -> Result<(), String> {
        let vm = self.vm.as_mut().ok_or("the Android VM is not running")?;
        let (base, max) = vm
            .memory
            .ok_or("the Android VM keeps the size it started with")?;
        let mb = mb.clamp(base, max);
        vm.control
            .send(Request::VirtioMem {
                bytes: virtio_mem_bytes(base, mb),
            })
            .map_err(|e| format!("the Android VM's memory could not be set to {mb} MiB: {e}"))
    }

    fn memory_growth_failed(&mut self, why: &str) {
        // The guest keeps the memory it has, and later VMs are made at their starting size.
        self.grow = false;
        eprintln!(
            "uid:{}: memory: the Android VM cannot grow on this computer ({why}); it keeps the \
             memory it started with",
            self.account.uid
        );
    }
}
