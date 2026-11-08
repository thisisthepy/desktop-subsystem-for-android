//! The macOS agent: the local API on the user's socket, and the user's Android VM behind it.
//!
//! One agent runs per user, as that user (see `launchd`), so it serves exactly one user:
//! the socket lives in the user's own DSA folder with mode 0600, and a connection from any
//! other user ID is refused all the same.
//!
//! The agent reaches the guest through the VM's vsock device. The control channel, the log
//! port and the file server are served here, as the Windows service serves them. The other
//! ports the guest connects to can only be accepted by the process that owns the VM, so the
//! agent relays each such connection to a Unix socket, `<user folder>/vsock/<port>.sock`
//! ([`relay_socket`]): the graphics ports to the window manager (`dsa-wm`), and the device
//! ports (sound, video, sensors, cameras) to the user's device lender (`dsa-devices`). A
//! connection goes only to a socket the user owns, in a folder the user owns.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::device_identity::{BootIdentity, FileDeviceIdentities, ImageBuild};
use dsa_host_core::diagnostics::HostFile;
use dsa_host_core::guest_display::{display_or_smallest, with_display_size};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{HostApiService, PerUser, ServiceFacts, UserAndroid};
use dsa_host_core::session::Session;
use dsa_host_core::settings::{FileSettings, SettingsStore};
use dsa_host_core::update::service::{
    check_alerting, next_image_check, CheckNow, StoreVersions, UpdateNotices, UpdatingAndroid,
};
use dsa_host_core::update::store::{ImageStore, Recovery};
use dsa_host_core::window_manager::{WindowManagerHost, WindowManagerKeeper};
use dsa_host_unix::socket::UnixLocalListener;
use dsa_transport::Listener;

use crate::android_vm::{BootPlan, MacVmLauncher, VmSlot};
use crate::fetch::CurlFetch;
use crate::host_config::{MacHostConfig, UserFolders};
use crate::host_memory::{MacHostMemory, VmProcess};
use crate::user_disk::{bring_disk_to, ensure_pstore_disk, raw_disk};
use crate::vm_config::{image_files, kernel_fits, mac_address_for, Disk, GuestArch, MacVmConfig};
use crate::vsock::{Framing, VsockListener, VsockStream};
use crate::vz::PortChannels;

pub use crate::host_config::{relay_socket, DEVICE_PORTS, GRAPHICS_PORTS, RELAYED_PORTS};

/// The user this agent runs as, as the local API names its callers.
pub fn local_user() -> String {
    format!("uid:{}", dsa_host_unix::socket::current_uid())
}

/// The total memory of this Mac, in MiB.
fn total_memory_mb() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    // SAFETY: hw.memsize is a 64-bit integer, and the buffer holds one.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut bytes as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0).then_some(bytes / (1024 * 1024))
}

/// Keeps the window manager running. Installed, it has a launchd agent of its own, which is
/// asked to start it (`launchctl kickstart`), so it runs with its Mach service; run by hand,
/// it is started as a child of the agent. It runs when its socket answers.
struct ProgramWindowManager {
    program: PathBuf,
    /// The window manager's launchd service target, when its agent is installed.
    launchd: Option<String>,
    /// The socket the window hosts dial, which the window manager answers on.
    socket: PathBuf,
    child: Option<Child>,
}

impl WindowManagerHost for ProgramWindowManager {
    fn running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            if matches!(child.try_wait(), Ok(None)) {
                return true;
            }
            self.child = None;
        }
        UnixStream::connect(&self.socket).is_ok()
    }

    fn start(&mut self) -> Result<(), String> {
        if let Some(target) = &self.launchd {
            let kickstart = ["kickstart".to_string(), target.clone()];
            return crate::launchd::launchctl(&kickstart);
        }
        let child = Command::new(&self.program).spawn().map_err(|e| {
            let program = self.program.display();
            format!("the window manager {program} could not start: {e}")
        })?;
        self.child = Some(child);
        Ok(())
    }
}

/// The window manager to keep running: the one the configuration names, else the one
/// installed beside this program. `None` runs Android without windows.
fn window_manager(config: &MacHostConfig) -> Option<ProgramWindowManager> {
    let program = config.window_manager.clone().or_else(|| {
        let this = std::env::current_exe().ok()?;
        crate::launchd::window_manager_beside(&this)
    })?;
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let uid = dsa_host_unix::socket::current_uid();
    // The agent runs the installed program, so a program the configuration names is run
    // directly.
    let agent = config.window_manager.is_none() && crate::launchd::wm_plist_path(&home).exists();
    let launchd = agent.then(|| crate::launchd::wm_service_target(uid));
    let support = crate::host_config::user_root_in(&home);
    Some(ProgramWindowManager {
        program,
        launchd,
        socket: crate::compositor::window_host::window_manager_socket(&support),
        child: None,
    })
}

/// The image the next boot uses: its folder and version.
fn current_image(
    config: &MacHostConfig,
    store: Option<&ImageStore>,
) -> Result<(PathBuf, String), String> {
    if let Some(store) = store {
        let current = store.state().map_err(|e| e.to_string())?.current;
        return Ok((store.image_dir(&current.version), current.version));
    }
    let dir = config
        .image_dir
        .clone()
        .ok_or("the configuration names no image to boot")?;
    let version = std::fs::read_to_string(dir.join("image.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|manifest| manifest["version"].as_str().map(str::to_string))
        .unwrap_or_else(|| "local".into());
    Ok((dir, version))
}

/// Removes the raw copies of images other than `keep`.
fn prune_image_cache(user: &UserFolders, keep: &str) {
    let Some(cache) = user.image_cache(keep).parent().map(Path::to_path_buf) else {
        return;
    };
    if let Ok(entries) = std::fs::read_dir(&cache) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy() != keep {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Makes the user's disk hold image `version` from its template in `dir`, as a fresh disk
/// for a data reset or at a first boot.
fn bring_user_disk(user: &UserFolders, dir: &Path, version: &str) -> Result<(), String> {
    let files = image_files(dir, GuestArch::HOST)?;
    let template = raw_disk(&files.disk_template, &user.image_cache(version))?;
    bring_disk_to(&user.disk(), &template, version)
}

/// Builds each boot's plan.
struct Planner {
    config: MacHostConfig,
    user: UserFolders,
    name: String,
    store: Option<Arc<ImageStore>>,
    settings: Arc<dyn SettingsStore>,
    identity: Arc<BootIdentity>,
}

impl Planner {
    fn plan(&self) -> Result<BootPlan, String> {
        let (dir, version) = current_image(&self.config, self.store.as_deref())?;
        let files = image_files(&dir, GuestArch::HOST)?;
        let mut head = vec![0u8; 0x1000];
        let read = std::fs::File::open(&files.kernel)
            .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
            .map_err(|e| format!("reading the kernel {}: {e}", files.kernel.display()))?;
        if !kernel_fits(&head[..read], GuestArch::HOST) {
            return Err(format!(
                "{} is not a kernel Apple's Virtualization framework boots on this Mac (an \
                 uncompressed arm64 Image on Apple silicon, a bzImage on Intel)",
                files.kernel.display()
            ));
        }
        let cache = self.user.image_cache(&version);
        let template = raw_disk(&files.disk_template, &cache)?;
        // A disk holding a newer Android's data than this image, after the user chose an older
        // version and agreed to erase it, starts afresh.
        if let Some(store) = &self.store {
            let major = store
                .state()
                .map_err(|e| e.to_string())?
                .current
                .android_major;
            let disk = self.user.disk();
            dsa_host_core::data_reset::erase_if_newer(
                &disk,
                &disk.with_extension("image-version"),
                major,
            )?;
        }
        bring_disk_to(&self.user.disk(), &template, &version)?;
        prune_image_cache(&self.user, &version);
        let mut disks = Vec::new();
        if let Some(system) = &files.system_disk {
            disks.push(Disk {
                path: raw_disk(system, &cache)?,
                read_only: true,
            });
        }
        disks.push(Disk {
            path: self.user.disk(),
            read_only: false,
        });
        if self.config.pstore {
            ensure_pstore_disk(&self.user.pstore_disk())?;
            disks.push(Disk {
                path: self.user.pstore_disk(),
                read_only: false,
            });
        }
        let cmdline = crate::cmdline::for_virtualization_framework(
            &files.kernel_cmdline,
            &self.config.kernel_cmdline,
            self.config.pstore,
        );
        let cmdline = with_display_size(&cmdline, display_or_smallest(None));
        let settings = self.settings.get(&self.name).ok();
        let limit = settings.as_ref().map_or(0, |s| s.memory_limit_mb);
        let (start, max) = self.config.memory_mb(limit, total_memory_mb());
        let max = max.min(crate::vz::max_memory_mb()).max(start);
        eprintln!("memory: Android starts with {start} MiB and may grow to {max} MiB");
        let mut listen_ports = vec![
            self.config.control_port,
            self.config.log_port,
            dsa_host_core::file_share::SHARE_PORT,
        ];
        listen_ports.extend(RELAYED_PORTS);
        let vm = MacVmConfig {
            kernel: files.kernel.clone(),
            initrd: files.initrd.clone(),
            cmdline,
            memory_max_mb: max,
            memory_start_mb: start,
            processors: self.config.processors,
            disks,
            console_log: None,
            network: self.config.network,
            mac_address: Some(mac_address_for(&self.name)),
            listen_ports,
        };
        // The user's device identity on this Mac, read again at each start.
        let properties = self
            .identity
            .boot_properties(ImageBuild::from_image_dir(&dir).as_ref());
        Ok(BootPlan {
            vm,
            properties: Some(properties),
            bootconfig: dsa_host_core::arm_translation::bootconfig_for(
                &*self.settings,
                &self.name,
                Some(dir.as_path()),
            ) + &dsa_host_core::gapps::bootconfig_for(&*self.settings, &self.name),
        })
    }
}

/// Relays each of the guest's connections to `port` to the socket of the user's program that
/// serves it: the window manager for graphics, the device lender for the devices. A socket
/// that is missing, or that is not the user's own, takes nothing: the guest's connection is
/// closed, which its HAL reads as a device that is not there.
fn relay_port(listener: VsockListenerRaw, socket: PathBuf, stop: Arc<AtomicBool>) {
    let owner = dsa_host_unix::socket::current_uid();
    let mut warned = false;
    while !stop.load(Ordering::Relaxed) {
        let fd = {
            let rx = listener.lock().unwrap_or_else(|e| e.into_inner());
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(fd) => fd,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        let guest = UnixStream::from(fd);
        let host = dsa_host_unix::device_socket::owned_by(&socket, owner)
            .and_then(|()| UnixStream::connect(&socket).map_err(|e| e.to_string()));
        let host = match host {
            Ok(host) => {
                warned = false;
                host
            }
            Err(why) => {
                // Said once until it works again: a HAL that retries must not fill the log.
                if !warned {
                    eprintln!(
                        "relay: the guest's connection is dropped, nothing of this user's                          listens on {} ({why})",
                        socket.display()
                    );
                    warned = true;
                }
                continue;
            }
        };
        pump(guest, host);
    }
}

type VsockListenerRaw = Arc<Mutex<std::sync::mpsc::Receiver<std::os::fd::OwnedFd>>>;

/// Copies bytes both ways between `a` and `b` until either side ends.
fn pump(a: UnixStream, b: UnixStream) {
    let pair = [(a.try_clone(), b.try_clone()), (Ok(b), Ok(a))];
    for (from, to) in pair {
        let (Ok(mut from), Ok(mut to)) = (from, to) else {
            continue;
        };
        let _ = thread::Builder::new()
            .name("dsa-relay".into())
            .spawn(move || {
                let _ = std::io::copy(&mut from, &mut to);
                let _ = to.shutdown(std::net::Shutdown::Both);
                let _ = from.shutdown(std::net::Shutdown::Both);
            });
    }
}

/// Opens the update store when the configuration names one.
fn open_store(config: &MacHostConfig, build: &str) -> Option<Arc<ImageStore>> {
    let (Some(root), Some(first)) = (&config.images_dir, &config.first_image) else {
        return None;
    };
    let store = match ImageStore::open_as(root, first, build) {
        Ok(store) => store,
        Err(why) => {
            eprintln!("{build}: image updates disabled: {why}");
            return None;
        }
    };
    match store.recover(first) {
        Ok(Recovery::Moved { broken, current }) => eprintln!(
            "{build}: the image {} has missing or broken files; using {} instead",
            broken.version, current.version
        ),
        Ok(_) => {}
        Err(why) => eprintln!("{build}: checking the current image: {why}"),
    }
    Some(Arc::new(store))
}

/// How soon the agent tries again while it has no image to boot.
const FIRST_IMAGE_RETRY: Duration = Duration::from_secs(300);

/// Checks the release list a minute after the agent starts and then every
/// `check_interval_hours`, until `stop` is set.
fn check_for_updates(
    config: MacHostConfig,
    store: Arc<ImageStore>,
    notices: Arc<UpdateNotices>,
    check_now: Arc<CheckNow>,
    settings: Arc<dyn SettingsStore>,
    build: String,
    stop: Arc<AtomicBool>,
) {
    let Some(update) = config.update.clone() else {
        return;
    };
    let keys = match update.keys() {
        Ok(keys) => keys,
        Err(why) => return eprintln!("{build}: not checking for updates: {why}"),
    };
    let interval = Duration::from_secs(update.check_interval_hours.max(1) * 3600);
    let no_image = || {
        store
            .state()
            .map(|state| !store.has_image(&state.current.version))
            .unwrap_or(false)
    };
    let mut next = if no_image() {
        Instant::now()
    } else {
        Instant::now() + Duration::from_secs(60)
    };
    while !stop.load(Ordering::Relaxed) {
        next = next_image_check(next, Instant::now(), !no_image(), FIRST_IMAGE_RETRY);
        // A user who chose another Android version waits for it now, not at the next turn.
        if check_now.take() {
            next = Instant::now();
        }
        if Instant::now() < next {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        if !no_image() && !store.switch_pending() && !settings.machine().image_updates {
            next = Instant::now() + Duration::from_secs(60);
            continue;
        }
        let mut announce = |download: &dsa_host_core::update::service::Download| {
            if download.done == 0 {
                eprintln!("{build}: {}", download.notice());
            }
            notices.set(download.notice());
        };
        match check_alerting(
            &store,
            &CurlFetch,
            &update.list_url,
            &keys,
            &notices,
            &mut announce,
        ) {
            Ok(Some(notice)) => {
                eprintln!("{build}: {notice}");
                notices.set(notice);
            }
            Ok(None) => {}
            Err(why) => eprintln!("{build}: checking for updates: {why}"),
        }
        next = Instant::now()
            + if no_image() {
                FIRST_IMAGE_RETRY
            } else {
                interval
            };
    }
}

/// Everything the user's Android needs that outlives one call.
struct Shared {
    config: MacHostConfig,
    user: UserFolders,
    build: String,
    store: Option<Arc<ImageStore>>,
    notices: Arc<UpdateNotices>,
    settings: Arc<dyn SettingsStore>,
    process: VmProcess,
    stop: Arc<AtomicBool>,
}

/// The user's Android: their VM and session.
fn create_android(shared: &Shared, name: &str) -> Result<Arc<dyn UserAndroid>, String> {
    let config = &shared.config;
    let channels = Arc::new(PortChannels::default());
    let boot_identity = BootIdentity::new(
        Arc::new(FileDeviceIdentities::new(config.settings_dir(&shared.user))),
        name,
        Arc::new(crate::computer_identity::read),
    );
    let planner = Planner {
        config: config.clone(),
        user: shared.user.clone(),
        name: name.to_string(),
        store: shared.store.clone(),
        settings: Arc::clone(&shared.settings),
        identity: Arc::clone(&boot_identity),
    };
    let launcher = MacVmLauncher::new(
        Box::new(move || planner.plan()),
        Arc::clone(&channels),
        shared.user.boot_dir(),
        Some(shared.user.console_log()),
    )
    .with_process(shared.process.clone());
    let launcher = match window_manager(config) {
        Some(window_manager) => {
            let mut keeper = WindowManagerKeeper::new(window_manager);
            launcher.with_prerequisite(Box::new(move || keeper.check(Instant::now())))
        }
        None => launcher,
    };
    let slot: VmSlot = launcher.slot();
    let control = VsockListener::new(channels.receiver(config.control_port), Framing::Framed);
    let log = VsockListener::new(channels.receiver(config.log_port), Framing::Framed);
    let share = VsockListener::new(
        channels.receiver(dsa_host_core::file_share::SHARE_PORT),
        Framing::Raw,
    );
    for port in RELAYED_PORTS {
        let (rx, socket, stop) = (
            channels.receiver(port),
            relay_socket(&shared.user, port),
            Arc::clone(&shared.stop),
        );
        if let Err(why) = thread::Builder::new()
            .name(format!("dsa-relay-{port}"))
            .spawn(move || relay_port(rx, socket, stop))
        {
            eprintln!("relay: port {port} cannot be relayed: {why}");
        }
    }
    let session_config = config.session_config();
    let handshake = ControlHandshake::new(&shared.build, session_config.connect_timeout);
    let session = Session::new(
        launcher,
        Box::new(control) as Box<dyn Listener>,
        handshake,
        session_config,
    );
    session.serve_log_port(Box::new(log), &shared.build);
    // The Mac's name becomes the guest's device name.
    session.set_device_name_source(Arc::new(move || boot_identity.device_name()));
    session.serve_file_shares(Box::new(share), Arc::new(user_folders()));
    let dial = |port: u32| {
        let slot = slot.clone();
        Arc::new(move |_vm_id: &str| {
            slot.connect(port, Duration::from_secs(5)).map(|fd| {
                Box::new(VsockStream::from_fd(fd))
                    as Box<dyn dsa_host_core::adb_bridge::BridgeStream>
            })
        })
    };
    session.set_adb_connect(dial(dsa_host_core::adb_bridge::GUEST_ADB_PORT));
    // Developer mode's port is attached to the user's adb server, and advertised over mDNS on
    // the loopback address, so adb finds it without `adb connect`.
    session.set_adb_server(crate::adb_discovery::adb_server());
    session.set_adb_advertise(crate::adb_discovery::advertiser());
    session.set_android_files_connect(dial(dsa_host_core::file_share::ANDROID_FILES_PORT));
    let session = Arc::new(session);
    // The guest's display refreshes as fast as the Mac's fastest display, and at 60 Hz in Low
    // Power Mode.
    let watched = Arc::downgrade(&session);
    if let Err(why) = thread::Builder::new()
        .name("dsa-display-power".into())
        .spawn(move || crate::display_power::follow(watched))
    {
        eprintln!("display: cannot follow Low Power Mode and the displays' refresh rate: {why}");
    }
    Ok(match &shared.store {
        None => session as Arc<dyn UserAndroid>,
        Some(store) => Arc::new(UpdatingAndroid {
            session,
            store: Arc::clone(store),
            notices: Arc::clone(&shared.notices),
            boot_timeout: Duration::from_secs(
                config.update.as_ref().map_or(300, |u| u.boot_timeout_secs),
            ),
            trial_running: Arc::new(AtomicBool::new(false)),
            rollback: Default::default(),
        }),
    })
}

/// The user's own folders, shared with their Android. The agent runs as the user, so the
/// file system checks each access against the user's own rights.
fn user_folders() -> crate::devices::shared_folders::MacFolders {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    crate::devices::shared_folders::MacFolders { home }
}

/// Applies the user's saved choices to their Android, as the Windows service does when it
/// creates one.
fn apply_settings(
    android: &Arc<dyn UserAndroid>,
    settings: Option<dsa_host_core::settings::Settings>,
    build: &str,
) {
    android.freeze_when_idle(settings.as_ref().is_some_and(|s| s.freeze));
    android.share_clipboard(settings.as_ref().is_none_or(|s| s.clipboard));
    android.links_in_android(settings.as_ref().is_some_and(|s| s.links_in_android));
    android.share_folders(
        settings.as_ref().is_none_or(|s| s.file_sharing),
        settings
            .as_ref()
            .map_or_else(dsa_host_core::file_share::all_folders, |s| {
                s.shared_folders.clone()
            }),
    );
    android.adb_auto_connect(settings.as_ref().is_none_or(|s| s.adb_auto_connect));
    if let Some(settings) = settings.as_ref().filter(|s| s.developer_mode) {
        if let Err(why) = android.developer_mode(true, settings.adb_port) {
            eprintln!("{build}: developer mode: {why}");
        }
    }
}

/// Serves the local API for the user whose home is `home` until `stop` is set, then stops
/// their Android.
pub fn serve(
    config: MacHostConfig,
    home: &Path,
    build: &str,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let user = config.user(home);
    for dir in [
        &user.root,
        &user.logs(),
        &user.boot_dir(),
        &user.root.join("vsock"),
    ] {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    // The user's folder holds their Android and the socket: only they may enter it.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&user.root, std::fs::Permissions::from_mode(0o700));
    }
    // Initrds a stopped start left behind carry an old secret.
    if let Ok(entries) = std::fs::read_dir(user.boot_dir()) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let socket = config.socket(&user);
    let listener = UnixLocalListener::bind(&socket, 0o600)
        .map_err(|e| format!("opening the socket {}: {e}", socket.display()))?;
    eprintln!("{build}: serving the local API on {}", socket.display());
    if !crate::vz::supported() {
        eprintln!(
            "{build}: this Mac reports no support for virtual machines; Android cannot start"
        );
    }
    // A computer whose settings record no release channel yet starts on the one this version
    // was published on, and keeps it through upgrades.
    let files = FileSettings::new(config.settings_dir(&user)).with_first_channel(
        dsa_host_core::update::program::ReleaseChannel::of_version(crate::VERSION),
    );
    if let Err(why) = files.keep_first_channel() {
        eprintln!("{build}: recording the release channel failed: {why}");
    }
    let settings: Arc<dyn SettingsStore> = Arc::new(files);
    let store = open_store(&config, build);
    let notices = Arc::new(UpdateNotices::default());
    let check_now = Arc::new(CheckNow::default());
    if let Some(store) = &store {
        let (config, store, notices, check, settings, worker, stop) = (
            config.clone(),
            Arc::clone(store),
            Arc::clone(&notices),
            Arc::clone(&check_now),
            Arc::clone(&settings),
            build.to_string(),
            Arc::clone(&stop),
        );
        if let Err(why) = thread::Builder::new()
            .name("dsa-updates".into())
            .spawn(move || check_for_updates(config, store, notices, check, settings, worker, stop))
        {
            eprintln!("{build}: starting the update check failed: {why}");
        }
    }
    let process = VmProcess::default();
    let shared = Arc::new(Shared {
        config: config.clone(),
        user: user.clone(),
        build: build.to_string(),
        store: store.clone(),
        notices,
        settings: Arc::clone(&settings),
        process: process.clone(),
        stop: Arc::clone(&stop),
    });
    let me = local_user();
    let factory = Arc::clone(&shared);
    let androids = Arc::new(PerUser::new(move |name: &str| {
        if name != me {
            return Err(format!(
                "this agent serves {me} only; {name} has an agent of their own"
            ));
        }
        let android = create_android(&factory, name)?;
        apply_settings(&android, factory.settings.get(name).ok(), &factory.build);
        Ok(android)
    }));
    let facts = ServiceFacts {
        version: crate::VERSION.into(),
        log_dir: Some(user.logs().to_string_lossy().into_owned()),
        program_updates_available: false,
        // No updater of its own here, so no release channel to follow.
        update_channel_available: false,
        image_updates_available: store.is_some() && config.update.is_some(),
        android_version: config.first_image.as_ref().map(|i| i.version.clone()),
    };
    let reset_shared = Arc::clone(&shared);
    let reset = dsa_host_core::data_reset::TemplateDataDisk::new(
        {
            let disk = user.disk();
            Box::new(move |_: &str| Ok(disk.clone()))
        },
        Box::new(move |disk: &Path| {
            let (dir, version) =
                current_image(&reset_shared.config, reset_shared.store.as_deref())?;
            if disk != reset_shared.user.disk() {
                return Err(format!("{} is not this user's disk", disk.display()));
            }
            bring_user_disk(&reset_shared.user, &dir, &version)
        }),
    )
    .with_beside("image-version");
    let logs_user = user.clone();
    let identity_shared = Arc::clone(&shared);
    let mut service = HostApiService::new(androids.clone(), build)
        .with_data_reset(Arc::new(reset))
        .with_settings(Arc::clone(&settings), facts)
        .with_host_memory(Arc::new(MacHostMemory { vm: process }))
        .with_widget_placements(Arc::new(dsa_host_core::widgets::FileWidgetPlacements::new(
            config.settings_dir(&user),
        )))
        .with_host_logs(Arc::new(move |_: &str| {
            vec![
                HostFile {
                    name: "host/dsa-hostd.log".into(),
                    path: logs_user.agent_log(),
                    only_user: None,
                },
                HostFile {
                    name: "host/android-console.log".into(),
                    path: logs_user.console_log(),
                    only_user: None,
                },
                HostFile {
                    name: "host/android-console.1.log".into(),
                    path: logs_user.console_log().with_extension("1.log"),
                    only_user: None,
                },
            ]
        }))
        .with_device_identities(
            Arc::new(FileDeviceIdentities::new(config.settings_dir(&user))),
            Arc::new(move |_: &str| {
                current_image(&identity_shared.config, identity_shared.store.as_deref())
                    .ok()
                    .and_then(|(dir, _)| ImageBuild::from_image_dir(&dir))
            }),
        )
        // The defaults the settings show are what this Mac says it is now.
        .with_computer_identity(Arc::new(crate::computer_identity::read))
        // An arm64 image runs ARM apps itself and has no default translator; this reports
        // whatever the image says, which for every image this host boots is none.
        .with_default_arm_translation({
            let shared = Arc::clone(&shared);
            Arc::new(move |_: &str| {
                current_image(&shared.config, shared.store.as_deref())
                    .ok()
                    .and_then(|(dir, _)| dsa_host_core::arm_translation::default_package(&dir))
            })
        });
    // Google Play, for a user who asks: downloaded from LiteGapps and made ready in the
    // user's own folder.
    service = service.with_gapps(dsa_host_core::gapps::Source::new(
        Arc::new(CurlFetch),
        config.settings_dir(&user).with_file_name("gapps"),
    ));
    if let Some(store) = &store {
        // The agent keeps this user's own images, so the choice is theirs alone.
        service = service.with_android_versions(Arc::new(StoreVersions {
            store: Arc::clone(store),
            notices: Arc::clone(&shared.notices),
            check_now: Arc::clone(&check_now),
            shared_by_users: false,
        }));
        let store = Arc::clone(store);
        service = service.with_android_version(Arc::new(move || {
            store.state().ok().map(|state| state.current.version)
        }));
    }
    let service = Arc::new(service);
    service.serve(Box::new(listener), Arc::clone(&stop));
    eprintln!("{build}: stopping Android");
    let grace = Duration::from_secs(config.stop_timeout_secs + 15);
    let not_stopped = dsa_host_core::host_api::UserAndroids::stop_all(&*androids, grace);
    if not_stopped.is_empty() {
        Ok(())
    } else {
        Err("Android's VM did not stop".into())
    }
}
