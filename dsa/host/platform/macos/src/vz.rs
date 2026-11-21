//! The VM itself, on Apple's Virtualization framework.
//!
//! A `VZVirtualMachine` belongs to the dispatch queue it was made with: every call to it, and
//! to the devices it owns, is made on that queue, and the framework calls back on it. Each VM
//! here gets a serial queue of its own; [`VirtualMachine`] makes each call there with
//! `exec_sync` and waits for completion handlers outside the queue, so the queue is never
//! blocked on itself.
//!
//! The framework needs the process to carry the `com.apple.security.virtualization`
//! entitlement; without it the configuration does not validate, and the error says so.
//!
//! Sockets: the guest's connections to the host's vsock ports arrive at a listener
//! delegate, which keeps a duplicate of each connection's file descriptor and hands it to
//! the port's channel ([`PortChannels`]). The duplicate stays usable after the framework's
//! connection object is gone.
#![allow(unused_unsafe)]

use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSArray, NSError, NSString, NSURL};
use objc2_virtualization::{
    VZBootLoader, VZDiskImageStorageDeviceAttachment, VZEntropyDeviceConfiguration,
    VZFileSerialPortAttachment, VZLinuxBootLoader, VZMACAddress,
    VZMemoryBalloonDeviceConfiguration, VZNATNetworkDeviceAttachment, VZNetworkDeviceAttachment,
    VZNetworkDeviceConfiguration, VZSerialPortAttachment, VZSerialPortConfiguration,
    VZSocketDeviceConfiguration, VZStorageDeviceAttachment, VZStorageDeviceConfiguration,
    VZVirtioBlockDeviceConfiguration, VZVirtioConsoleDeviceSerialPortConfiguration,
    VZVirtioEntropyDeviceConfiguration, VZVirtioNetworkDeviceConfiguration,
    VZVirtioSocketConnection, VZVirtioSocketDevice, VZVirtioSocketDeviceConfiguration,
    VZVirtioSocketListener, VZVirtioSocketListenerDelegate, VZVirtioTraditionalMemoryBalloonDevice,
    VZVirtioTraditionalMemoryBalloonDeviceConfiguration, VZVirtualMachine,
    VZVirtualMachineConfiguration, VZVirtualMachineDelegate, VZVirtualMachineState,
};

use crate::vm_config::MacVmConfig;

const MIB: u64 = 1024 * 1024;

/// How long the framework may take to start or stop the VM.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The text of an error the framework handed a completion handler, or `None` for success.
fn error_text(error: *mut NSError) -> Option<String> {
    // SAFETY: the framework passes a valid NSError or nil.
    unsafe { error.as_ref() }.map(|error| error.localizedDescription().to_string())
}

fn ns_error(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

fn file_url(path: &std::path::Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

/// Whether this Mac can run VMs at all (Apple silicon or a Mac with the hypervisor, not
/// itself a VM without nested virtualization).
pub fn supported() -> bool {
    unsafe { VZVirtualMachine::isSupported() }
}

/// The largest memory size the framework allows a VM, in MiB.
pub fn max_memory_mb() -> u64 {
    let bytes = unsafe { VZVirtualMachineConfiguration::maximumAllowedMemorySize() };
    bytes / MIB
}

/// Where the guest's connections to one host port arrive.
pub type Incoming = Arc<Mutex<Receiver<OwnedFd>>>;

/// The channels the guest's connections to each host port arrive on. They outlive each VM:
/// the session's listeners read from them across boots.
#[derive(Default)]
pub struct PortChannels {
    ports: Mutex<HashMap<u32, (Sender<OwnedFd>, Incoming)>>,
}

impl PortChannels {
    fn entry(&self, port: u32) -> (Sender<OwnedFd>, Incoming) {
        let mut ports = self.ports.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = ports.entry(port).or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            (tx, Arc::new(Mutex::new(rx)))
        });
        (tx.clone(), Arc::clone(rx))
    }

    /// Where port `port`'s connections arrive.
    pub fn receiver(&self, port: u32) -> Incoming {
        self.entry(port).1
    }

    fn sender(&self, port: u32) -> Sender<OwnedFd> {
        self.entry(port).0
    }

    /// Drops connections a previous VM left unaccepted.
    ///
    /// A receiver whose lock is held has a reader waiting on it, and a waiting reader takes
    /// whatever arrives, so nothing is left there to drop. Waiting for that lock instead
    /// never ends: the reader takes it again the moment it lets go, and the macOS mutex does
    /// not hand it to the thread that has waited longest.
    pub fn drain(&self) {
        let ports = self.ports.lock().unwrap_or_else(|e| e.into_inner());
        for (_, rx) in ports.values() {
            let rx = match rx.try_lock() {
                Ok(rx) => rx,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => continue,
            };
            while rx.try_recv().is_ok() {}
        }
    }
}

/// What each listener delegate holds: where its port's connections go.
pub struct DelegateIvars {
    connections: Mutex<Sender<OwnedFd>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "DSAVsockListenerDelegate"]
    #[ivars = DelegateIvars]
    struct ListenerDelegate;

    unsafe impl NSObjectProtocol for ListenerDelegate {}

    unsafe impl VZVirtioSocketListenerDelegate for ListenerDelegate {
        #[unsafe(method(listener:shouldAcceptNewConnection:fromSocketDevice:))]
        fn should_accept(
            &self,
            _listener: &VZVirtioSocketListener,
            connection: &VZVirtioSocketConnection,
            _device: &VZVirtioSocketDevice,
        ) -> Bool {
            // SAFETY: the connection's descriptor is open while the connection exists; the
            // duplicate is this process's own.
            let fd = unsafe { libc::dup(connection.fileDescriptor()) };
            if fd < 0 {
                return Bool::NO;
            }
            // SAFETY: a descriptor just duplicated, owned by nothing else.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let sender = self
                .ivars()
                .connections
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            Bool::new(sender.send(fd).is_ok())
        }
    }
);

impl ListenerDelegate {
    fn new(connections: Sender<OwnedFd>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars {
            connections: Mutex::new(connections),
        });
        // SAFETY: NSObject's init on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// What the VM's delegate holds: why the VM stopped, once the framework has said.
pub struct VmDelegateIvars {
    stopped: Arc<Mutex<Option<String>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "DSAVirtualMachineDelegate"]
    #[ivars = VmDelegateIvars]
    struct VmDelegate;

    unsafe impl NSObjectProtocol for VmDelegate {}

    unsafe impl VZVirtualMachineDelegate for VmDelegate {
        #[unsafe(method(guestDidStopVirtualMachine:))]
        fn guest_did_stop(&self, _vm: &VZVirtualMachine) {
            self.record("Android turned its VM off".into());
        }

        #[unsafe(method(virtualMachine:didStopWithError:))]
        fn did_stop_with_error(&self, _vm: &VZVirtualMachine, error: &NSError) {
            self.record(format!(
                "the Virtualization framework stopped the VM: {} (domain {}, code {})",
                ns_error(error),
                error.domain(),
                error.code()
            ));
        }
    }
);

impl VmDelegate {
    fn new(stopped: Arc<Mutex<Option<String>>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(VmDelegateIvars { stopped });
        // SAFETY: NSObject's init on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    fn record(&self, why: String) {
        // Said here as well, the moment it happens: the session asks for it only every so often.
        eprintln!("vm: {why}");
        *self
            .ivars()
            .stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(why);
    }
}

/// A running (or stopped) VM and the objects it keeps.
pub struct VirtualMachine {
    queue: DispatchRetained<DispatchQueue>,
    vm: Retained<VZVirtualMachine>,
    // The framework holds the delegate weakly.
    _delegate: Retained<VmDelegate>,
    /// Why the VM stopped by itself, as its delegate heard it.
    stopped: Arc<Mutex<Option<String>>>,
    // The listeners and their delegates: the framework holds the delegate weakly.
    _listeners: Vec<(Retained<VZVirtioSocketListener>, Retained<ListenerDelegate>)>,
}

// SAFETY: the framework's objects are only touched on `queue`, through `on_queue`.
unsafe impl Send for VirtualMachine {}
// SAFETY: as above; every method goes through the queue, which serialises them.
unsafe impl Sync for VirtualMachine {}

/// Builds the framework's configuration from `config`.
fn configuration(config: &MacVmConfig) -> Result<Retained<VZVirtualMachineConfiguration>, String> {
    unsafe {
        let boot = VZLinuxBootLoader::initWithKernelURL(
            VZLinuxBootLoader::alloc(),
            &file_url(&config.kernel),
        );
        boot.setInitialRamdiskURL(Some(&file_url(&config.initrd)));
        boot.setCommandLine(&NSString::from_str(&config.cmdline));

        let vm = VZVirtualMachineConfiguration::new();
        let boot: &VZBootLoader = &boot;
        vm.setBootLoader(Some(boot));
        vm.setCPUCount(config.processors as usize);
        vm.setMemorySize(config.memory_max_mb * MIB);

        let mut disks: Vec<Retained<VZStorageDeviceConfiguration>> = Vec::new();
        for disk in &config.disks {
            let attachment = VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
                VZDiskImageStorageDeviceAttachment::alloc(),
                &file_url(&disk.path),
                disk.read_only,
            )
            .map_err(|e| format!("attaching {}: {}", disk.path.display(), ns_error(&e)))?;
            let attachment: &VZStorageDeviceAttachment = &attachment;
            let block = VZVirtioBlockDeviceConfiguration::initWithAttachment(
                VZVirtioBlockDeviceConfiguration::alloc(),
                attachment,
            );
            disks.push(Retained::into_super(block));
        }
        vm.setStorageDevices(&NSArray::from_retained_slice(&disks));

        let sockets = VZVirtioSocketDeviceConfiguration::new();
        let sockets: Retained<VZSocketDeviceConfiguration> = Retained::into_super(sockets);
        vm.setSocketDevices(&NSArray::from_retained_slice(&[sockets]));

        let balloon = VZVirtioTraditionalMemoryBalloonDeviceConfiguration::new();
        let balloon: Retained<VZMemoryBalloonDeviceConfiguration> = Retained::into_super(balloon);
        vm.setMemoryBalloonDevices(&NSArray::from_retained_slice(&[balloon]));

        let entropy = VZVirtioEntropyDeviceConfiguration::new();
        let entropy: Retained<VZEntropyDeviceConfiguration> = Retained::into_super(entropy);
        vm.setEntropyDevices(&NSArray::from_retained_slice(&[entropy]));

        if config.network {
            let nat = VZNATNetworkDeviceAttachment::new();
            let nat: &VZNetworkDeviceAttachment = &nat;
            let adapter = VZVirtioNetworkDeviceConfiguration::new();
            adapter.setAttachment(Some(nat));
            if let Some(mac) = &config.mac_address {
                if let Some(address) =
                    VZMACAddress::initWithString(VZMACAddress::alloc(), &NSString::from_str(mac))
                {
                    adapter.setMACAddress(&address);
                }
            }
            let adapter: Retained<VZNetworkDeviceConfiguration> = Retained::into_super(adapter);
            vm.setNetworkDevices(&NSArray::from_retained_slice(&[adapter]));
        }

        if let Some(log) = &config.console_log {
            if let Some(folder) = log.parent() {
                let _ = std::fs::create_dir_all(folder);
            }
            let file = VZFileSerialPortAttachment::initWithURL_append_error(
                VZFileSerialPortAttachment::alloc(),
                &file_url(log),
                true,
            )
            .map_err(|e| {
                format!(
                    "opening the console log {}: {}",
                    log.display(),
                    ns_error(&e)
                )
            })?;
            let file: &VZSerialPortAttachment = &file;
            let console = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            console.setAttachment(Some(file));
            let console: Retained<VZSerialPortConfiguration> = Retained::into_super(console);
            vm.setSerialPorts(&NSArray::from_retained_slice(&[console]));
        }

        vm.validateWithError().map_err(|e| {
            format!(
                "the Virtualization framework refused the VM: {} (dsa-hostd must be signed \
                 with the com.apple.security.virtualization entitlement)",
                ns_error(&e)
            )
        })?;
        Ok(vm)
    }
}

/// Checks `config` with the framework without creating a VM.
pub fn validate(config: &MacVmConfig) -> Result<(), String> {
    config.validate()?;
    configuration(config).map(|_| ())
}

/// Passes something that is only used on the VM's queue across to it.
struct OnQueue<T>(T);
// SAFETY: the value is used only inside the queue's closure, one at a time.
unsafe impl<T> Send for OnQueue<T> {}

impl VirtualMachine {
    /// Creates the VM described by `config`, with listeners on its vsock ports that hand
    /// connections to `channels`. It is not started.
    pub fn create(config: &MacVmConfig, channels: &PortChannels) -> Result<VirtualMachine, String> {
        config.validate()?;
        let configuration = OnQueue(configuration(config)?);
        let queue = DispatchQueue::new("io.github.thisisthepy.dsa.vm", None);
        let senders: Vec<(u32, Sender<OwnedFd>)> = config
            .listen_ports
            .iter()
            .map(|&port| (port, channels.sender(port)))
            .collect();
        let stopped: Arc<Mutex<Option<String>>> = Arc::default();
        let delegate = VmDelegate::new(Arc::clone(&stopped));
        let mut made = None;
        {
            let made = &mut made;
            let queue_ref = OnQueue(&*queue);
            let delegate_ref = OnQueue(&*delegate);
            queue.exec_sync(move || {
                let configuration = configuration;
                let queue_ref = queue_ref;
                let delegate_ref = delegate_ref;
                let result = unsafe {
                    let vm = VZVirtualMachine::initWithConfiguration_queue(
                        VZVirtualMachine::alloc(),
                        &configuration.0,
                        queue_ref.0,
                    );
                    vm.setDelegate(Some(ProtocolObject::from_ref(delegate_ref.0)));
                    let device = vm.socketDevices().firstObject();
                    match device {
                        None => Err("the VM has no socket device".to_string()),
                        Some(device) => {
                            let device: Retained<VZVirtioSocketDevice> =
                                Retained::cast_unchecked(device);
                            let mut listeners = Vec::new();
                            for (port, sender) in senders {
                                let delegate = ListenerDelegate::new(sender);
                                let listener = VZVirtioSocketListener::new();
                                listener.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                                device.setSocketListener_forPort(&listener, port);
                                listeners.push((listener, delegate));
                            }
                            Ok((vm, listeners))
                        }
                    }
                };
                *made = Some(OnQueue(result));
            });
        }
        let (vm, listeners) = made
            .map(|m| m.0)
            .unwrap_or_else(|| Err("the VM's queue did not run".into()))?;
        Ok(VirtualMachine {
            queue,
            vm,
            _delegate: delegate,
            stopped,
            _listeners: listeners,
        })
    }

    /// Runs `work` on the VM's queue with the VM, and returns what it returns.
    fn on_queue<R: Send>(&self, work: impl FnOnce(&VZVirtualMachine) -> R + Send) -> R {
        let mut result = None;
        {
            let result = &mut result;
            let vm = OnQueue(&*self.vm);
            self.queue.exec_sync(move || {
                let vm = vm;
                *result = Some(work(vm.0));
            });
        }
        result.expect("exec_sync runs its work before returning")
    }

    /// Calls `call` on the VM's queue with a completion handler, and waits for the handler.
    fn with_completion(
        &self,
        what: &str,
        call: impl FnOnce(&VZVirtualMachine, &block2::DynBlock<dyn Fn(*mut NSError)>) + Send,
    ) -> Result<(), String> {
        let (tx, rx) = mpsc::channel::<Option<String>>();
        self.on_queue(move |vm| {
            let handler = RcBlock::new(move |error: *mut NSError| {
                let _ = tx.send(error_text(error));
            });
            call(vm, &handler);
        });
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(None) => Ok(()),
            Ok(Some(error)) => Err(format!("{what}: {error}")),
            Err(_) => Err(format!(
                "{what}: the Virtualization framework did not answer within {} seconds",
                CALL_TIMEOUT.as_secs()
            )),
        }
    }

    /// Starts the VM.
    pub fn start(&self) -> Result<(), String> {
        self.with_completion("starting the VM", |vm, handler| unsafe {
            vm.startWithCompletionHandler(handler)
        })
    }

    /// Asks the guest to shut down, as pressing a power button does.
    pub fn request_stop(&self) -> Result<(), String> {
        self.on_queue(|vm| unsafe {
            if !vm.canRequestStop() {
                return Err("the VM cannot be asked to stop now".to_string());
            }
            vm.requestStopWithError().map_err(|e| ns_error(&e))
        })
    }

    /// Stops the VM at once.
    pub fn force_stop(&self) -> Result<(), String> {
        let can_stop = self.on_queue(|vm| unsafe { vm.canStop() });
        if !can_stop {
            return Ok(());
        }
        self.with_completion("stopping the VM", |vm, handler| unsafe {
            vm.stopWithCompletionHandler(handler)
        })
    }

    /// The VM's state.
    pub fn state(&self) -> VZVirtualMachineState {
        self.on_queue(|vm| unsafe { vm.state() })
    }

    /// Whether the VM has stopped, by itself or with an error, and why when the framework
    /// said.
    pub fn ended(&self) -> Option<String> {
        let state = self.state();
        let what = if state == VZVirtualMachineState::Stopped {
            "the VM has stopped"
        } else if state == VZVirtualMachineState::Error {
            "the VM stopped with an error"
        } else {
            return None;
        };
        let why = self
            .stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Some(match why {
            Some(why) => format!("{what}: {why}"),
            None => what.to_string(),
        })
    }

    /// Pauses every processor of the VM, keeping its memory.
    pub fn pause(&self) -> Result<(), String> {
        self.with_completion("pausing the VM", |vm, handler| unsafe {
            vm.pauseWithCompletionHandler(handler)
        })
    }

    /// Lets a paused VM run again.
    pub fn resume(&self) -> Result<(), String> {
        self.with_completion("resuming the VM", |vm, handler| unsafe {
            vm.resumeWithCompletionHandler(handler)
        })
    }

    /// Sets how much memory the balloon leaves the guest, in MiB.
    pub fn set_memory_mb(&self, mb: u64) -> Result<(), String> {
        self.on_queue(move |vm| unsafe {
            let Some(balloon) = vm.memoryBalloonDevices().firstObject() else {
                return Err("the VM has no memory balloon".to_string());
            };
            let balloon: Retained<VZVirtioTraditionalMemoryBalloonDevice> =
                Retained::cast_unchecked(balloon);
            balloon.setTargetVirtualMachineMemorySize(mb * MIB);
            Ok(())
        })
    }

    /// Connects to the guest's vsock port `port`.
    pub fn connect(&self, port: u32, timeout: Duration) -> std::io::Result<OwnedFd> {
        let (tx, rx) = mpsc::channel::<Result<OwnedFd, String>>();
        self.on_queue(move |vm| unsafe {
            let Some(device) = vm.socketDevices().firstObject() else {
                let _ = tx.send(Err("the VM has no socket device".into()));
                return;
            };
            let device: Retained<VZVirtioSocketDevice> = Retained::cast_unchecked(device);
            let handler = RcBlock::new(
                move |connection: *mut VZVirtioSocketConnection, error: *mut NSError| {
                    // SAFETY: the framework passes a valid connection or nil.
                    let result = match (connection.as_ref(), error_text(error)) {
                        (Some(connection), None) => {
                            let fd = libc::dup(connection.fileDescriptor());
                            if fd < 0 {
                                Err(std::io::Error::last_os_error().to_string())
                            } else {
                                Ok(OwnedFd::from_raw_fd(fd))
                            }
                        }
                        (_, Some(error)) => Err(error),
                        (None, None) => Err("the guest gave no connection".into()),
                    };
                    let _ = tx.send(result);
                },
            );
            device.connectToPort_completionHandler(port, &handler);
        });
        match rx.recv_timeout(timeout) {
            Ok(Ok(fd)) => Ok(fd),
            Ok(Err(why)) => Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                format!("connecting to the guest's port {port}: {why}"),
            )),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("connecting to the guest's port {port} timed out"),
            )),
        }
    }
}
