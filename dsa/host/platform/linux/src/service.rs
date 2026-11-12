//! The DSA service on Linux: the local API on a Unix socket, and one Android VM per user
//! behind it.
//!
//! `dsa-hostd` runs this as a systemd system service (`dsa-hostd.service`). Each caller's
//! user ID, as the kernel reports it for the socket connection, picks the VM: the first
//! request of a user creates their disk and their session, and every later one reaches the
//! same session. The service runs as root so it can serve the vsock ports every guest shares
//! and make a user's LAN link; each user's crosvm, passt and files are the user's own.
//!
//! With `images_dir`, every VM boots the image the update store calls current, and with
//! `update` a background thread checks the signed release list and downloads new images. The
//! next start of a user's Android boots a new image on trial and goes back to the previous
//! one when it does not finish booting. DSA itself is updated by the system's package manager.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::clock_trace::{ClockTrace, HostClock};
use dsa_host_core::data_reset::TemplateDataDisk;
use dsa_host_core::device_identity::{BootIdentity, FileDeviceIdentities, ImageBuild};
use dsa_host_core::diagnostics::HostFile;
use dsa_host_core::guest_log::{LogFilter, LogWatch, DEFAULT_BUFFERS, WARN};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{
    HostApiService, NetworkStatus, PerUser, ServiceFacts, UserAndroid, UserAndroids,
};
use dsa_host_core::host_config::DEFAULT_MEMORY_MB;
use dsa_host_core::host_paths::HostPaths;
use dsa_host_core::memory::{summary_lines, HostMemory, LOG_INTERVAL};
use dsa_host_core::power::PowerHub;
use dsa_host_core::root_approval::RootApproval;
use dsa_host_core::session::{Session, SessionState};
use dsa_host_core::settings::{FileSettings, SettingsStore};
use dsa_host_core::update::service::{
    check_alerting, follow_waiting_image, next_image_check, CheckNow, Download, StoreVersions,
    UpdateNotices, UpdatingAndroid, WAITING_IMAGE_CHECK,
};
use dsa_host_core::update::store::{ImageStore, Recovery};
use dsa_host_core::window_manager::WindowManagerKeeper;
use dsa_host_unix::paths::{Layout, UnixHostPaths};
use dsa_host_unix::socket::UnixLocalListener;

use crate::config::HostConfig;
use crate::crosvm::guest_cid;
use crate::image::{bring_disk_to, image_files, make_disk, ImageFiles};
use crate::launcher::{BootPlan, LauncherSetup, LinuxVmLauncher, SharedNetworkStatus};
use crate::signs::BootSigns;
use crate::users::{uid_of, Account};
use crate::vsock::{connect_stream, VsockHub};

/// The boot property that makes the guest trace input, which the clock lines pair with.
pub const GUEST_INPUT_TRACE: &str = "androidboot.hardware.dsa_input_trace=1";

/// How soon the service tries again while it has no image to boot.
const FIRST_IMAGE_RETRY: Duration = Duration::from_secs(300);
/// How soon an update check that the settings turned off looks at them again.
const SETTINGS_RECHECK: Duration = Duration::from_secs(60);

/// Whether the guest booted with `cmdline` traces input.
pub fn guest_traces_input(cmdline: &str) -> bool {
    cmdline.split_whitespace().any(|w| w == GUEST_INPUT_TRACE)
}

/// The memory a user's VM starts with and may grow to, in MiB, for a user whose limit is
/// `limit_mb` (0 for automatic) on a computer with `total_mb`: a small start, as the guest
/// kernel keeps page records for all the memory it has, and growth to their limit or to the
/// computer's memory less what the host keeps.
pub fn memory_sizing(limit_mb: u32, total_mb: Option<u64>) -> (u32, Option<u32>) {
    let start = match limit_mb {
        0 => DEFAULT_MEMORY_MB,
        limit => limit.clamp(2048, DEFAULT_MEMORY_MB),
    };
    let max = total_mb
        .map(|total| dsa_host_core::memory::vm_memory_mb(total, limit_mb))
        .map(|max| u32::try_from(max).unwrap_or(u32::MAX))
        .filter(|max| *max > start);
    (start, max)
}

fn host_total_mb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    crate::devices::power::parse_meminfo(&text).map(|(total_kb, _)| total_kb / 1024)
}

/// The host's monotonic clock, for the clock lines.
struct MonotonicClock;

impl HostClock for MonotonicClock {
    fn now(&self) -> Duration {
        // SAFETY: timespec is plain data that clock_gettime fills in.
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        // SAFETY: a valid clock and a timespec to write.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
    }
}

/// What the service shares between users when it updates the image.
#[derive(Clone)]
pub struct Updates {
    pub store: Arc<ImageStore>,
    pub notices: Arc<UpdateNotices>,
    pub boot_timeout: Duration,
    pub trial_running: Arc<AtomicBool>,
    /// Set when a user chose another Android version, so the image is checked for at once.
    pub check_now: Arc<CheckNow>,
}

/// Where each user's files are.
pub fn host_paths() -> UnixHostPaths {
    UnixHostPaths::standard(
        Layout::Linux,
        Box::new(|user: &str| -> Result<PathBuf, String> {
            Ok(Account::of_uid(uid_of(user)?)?.home)
        }),
    )
}

/// The user's Android disk in their data folder `root`: the qcow2 disk an earlier DSA made, when
/// they have one, else the raw disk DSA makes now.
pub fn user_disk(root: &Path) -> PathBuf {
    let earlier = root.join("data.qcow2");
    if earlier.exists() {
        earlier
    } else {
        root.join("data.img")
    }
}

/// The files of one user's Android.
pub struct UserFiles {
    pub data_disk: PathBuf,
    pub pstore_disk: PathBuf,
    /// The disk of the add-ons the user installed, which their VM attaches read only.
    pub addon_disk: PathBuf,
    pub console_log: PathBuf,
    pub run_dir: PathBuf,
}

impl UserFiles {
    pub fn of(paths: &dyn HostPaths, user: &str) -> Result<UserFiles, String> {
        let root = paths.user_data(user)?;
        Ok(UserFiles {
            data_disk: user_disk(&root),
            pstore_disk: root.join("pstore.img"),
            addon_disk: root.join("addons.img"),
            console_log: paths.user_logs(user)?.join("android-console.log"),
            run_dir: paths.user_tmp(user)?,
        })
    }
}

fn settings_store(config: &HostConfig) -> Arc<dyn SettingsStore> {
    Arc::new(FileSettings::new(config.settings_dir()))
}

/// The service's settings: a computer whose settings record no release channel yet starts on
/// the channel of the repository entry the package wrote (else the one this version was
/// published on), and records it now so upgrades keep it; from then on the entries follow
/// the setting.
fn service_settings_store(config: &HostConfig, build: &str) -> Arc<dyn SettingsStore> {
    use crate::package_updates::{ChannelFollowingSettings, RepositoryEntries};
    use dsa_host_core::update::program::ReleaseChannel;
    let entries = RepositoryEntries::system();
    let first = entries
        .channel()
        .unwrap_or_else(|| ReleaseChannel::of_version(crate::VERSION));
    let files = FileSettings::new(config.settings_dir()).with_first_channel(first);
    match files.keep_first_channel() {
        Ok(channel) => eprintln!("{build}: DSA follows the {channel} release channel"),
        Err(why) => eprintln!("{build}: recording the release channel failed: {why}"),
    }
    let store = ChannelFollowingSettings::new(Arc::new(files), entries);
    match store.bring_entries_in_line() {
        Ok(changed) => {
            for path in changed {
                eprintln!(
                    "{build}: {} now follows the chosen release channel",
                    path.display()
                );
            }
        }
        Err(why) => eprintln!("{build}: {why}"),
    }
    Arc::new(store)
}

/// The image folder the next boot starts from, and its version.
fn current_image(
    config: &HostConfig,
    updates: Option<&Updates>,
) -> Result<(PathBuf, Option<String>), String> {
    match (updates, &config.image_dir) {
        (Some(updates), _) => {
            let current = updates.store.state().map_err(|e| e.to_string())?.current;
            Ok((
                updates.store.image_dir(&current.version),
                Some(current.version),
            ))
        }
        (None, Some(dir)) => {
            let version = std::fs::read_to_string(dir.join("image.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|m| m["version"].as_str().map(str::to_string));
            Ok((dir.clone(), version))
        }
        (None, None) => Err("the configuration names no Android image".into()),
    }
}

/// Reads Android's log while it boots, for what explains a boot that stalls.
fn watch_boot_signs(session: Weak<Session>, signs: Arc<Mutex<BootSigns>>) {
    let Some(events) = session.upgrade().map(|s| s.events()) else {
        return;
    };
    let mut watch: Option<LogWatch> = None;
    loop {
        let Some(running) = session.upgrade() else {
            return;
        };
        let booting = matches!(
            running.state(),
            SessionState::Starting | SessionState::Stalled
        );
        if booting && watch.is_none() {
            watch = Some(running.watch_log(LogFilter {
                buffers: DEFAULT_BUFFERS,
                tags: vec![("*".into(), WARN)],
            }));
        } else if !booting {
            watch = None;
        }
        drop(running);
        match &watch {
            Some(log) => {
                if let Ok(entry) = log.entries.recv_timeout(Duration::from_millis(500)) {
                    let mut signs = signs.lock().unwrap_or_else(|e| e.into_inner());
                    for entry in std::iter::once(entry).chain(log.entries.try_iter()) {
                        signs.read_line(&format!("{}: {}", entry.tag, entry.message));
                    }
                }
            }
            None => match events.recv_timeout(Duration::from_secs(1)) {
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            },
        }
    }
}

fn vm_cid(vm_id: &str) -> std::io::Result<u32> {
    vm_id.parse().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{vm_id} is not a VM's vsock address"),
        )
    })
}

/// The user's Android: their disk, VM and session, created on their first request.
pub fn create_user_android(
    config: &HostConfig,
    build: &str,
    user: &str,
    updates: Option<&Updates>,
    power: Option<&PowerHub>,
    network_status: SharedNetworkStatus,
) -> Result<Arc<dyn UserAndroid>, String> {
    let uid = uid_of(user)?;
    let account = Account::of_uid(uid)?;
    let files = UserFiles::of(&host_paths(), user)?;
    let crosvm = match &config.crosvm {
        Some(crosvm) => crosvm.clone(),
        None => crate::network::find_program("crosvm")
            .unwrap_or_else(|| PathBuf::from("/usr/lib/dsa/bin/crosvm")),
    };
    let store = settings_store(config);
    let trace_on = Arc::new(AtomicBool::new(false));
    let plan = {
        let (config, updates, account, store) = (
            config.clone(),
            updates.cloned(),
            account.clone(),
            Arc::clone(&store),
        );
        let (disk, run_dir, traced, user) = (
            files.data_disk.clone(),
            files.run_dir.clone(),
            Arc::clone(&trace_on),
            user.to_string(),
        );
        Box::new(move || -> Result<BootPlan, String> {
            let (dir, version) = current_image(&config, updates.as_ref())?;
            let mut image = image_files(&dir, config.kernel.as_deref(), &config.kernel_cmdline)?;
            // Every app starts at the density of the user's main monitor, which their window
            // manager records, so the windows that open there ask for no other.
            image.kernel_cmdline =
                crate::monitor_density::with_boot_density(&image.kernel_cmdline, &run_dir);
            // crosvm attaches raw disks, so a VHDX system disk is read into one once.
            image.system_image = image
                .system_image
                .as_deref()
                .map(crate::image::raw_disk)
                .transpose()?;
            // A disk holding a newer Android's data than this image, after the user chose an
            // older version and agreed to erase it, starts afresh.
            if let Some(updates) = &updates {
                let major = updates
                    .store
                    .state()
                    .map_err(|e| e.to_string())?
                    .current
                    .android_major;
                dsa_host_core::data_reset::erase_if_newer(
                    &disk,
                    &crate::image::version_file(&disk),
                    major,
                )?;
            }
            match (&image.disk_template, &version) {
                (Some(template), Some(version)) => bring_disk_to(
                    &account,
                    &disk,
                    template,
                    image.system_image.is_some(),
                    version,
                )?,
                (template, _) if !disk.exists() => make_disk(
                    &account,
                    &disk,
                    template.as_deref(),
                    config.data_disk_size_gb,
                )?,
                _ => {}
            }
            let (memory_mb, memory_max_mb) = match config.memory_mb {
                Some(fixed) => (fixed, None),
                None => {
                    let limit = store.get(&user).map(|s| s.memory_limit_mb).unwrap_or(0);
                    memory_sizing(limit, host_total_mb())
                }
            };
            eprintln!(
                "{user}: memory: Android starts with {memory_mb} MiB and may grow to {}",
                memory_max_mb.map_or("its start".to_string(), |mb| format!("{mb} MiB"))
            );
            traced.store(guest_traces_input(&image.kernel_cmdline), Ordering::Relaxed);
            Ok(BootPlan {
                files: image,
                image_version: version,
                memory_mb,
                memory_max_mb,
            })
        })
    };
    let cid = guest_cid(uid);
    // What this computer says it is, with what the user chose, read at each start.
    let boot_identity = BootIdentity::new(
        Arc::new(FileDeviceIdentities::new(config.settings_dir())),
        user,
        Arc::new(crate::computer_identity::read),
    );
    let mut launcher = LinuxVmLauncher::new(
        LauncherSetup {
            account: account.clone(),
            name: format!("{}-{uid}", config.vm_name),
            guest_cid: cid,
            crosvm,
            sandbox: config.sandbox,
            processors: config.processors,
            page_reporting_order: config.page_reporting_order,
            data_disk: files.data_disk.clone(),
            pstore_disk: Some(files.pstore_disk.clone()),
            addon_disk: Some(files.addon_disk.clone()),
            run_dir: files.run_dir.clone(),
            console_log: files.console_log.clone(),
            grow_memory: config.grow_memory,
        },
        plan,
    )
    .with_bootconfig({
        // An ARM translator (the package the user installed, else the image's default one) is
        // used only while the user's setting is on; the Google apps the user installed, while
        // they are installed. Their images are on the add-on disk this boot attaches, which
        // the launcher has readied by now.
        let (store, user) = (Arc::clone(&store), user.to_string());
        let addons = files.addon_disk.clone();
        Box::new(move |image: &ImageFiles| {
            use dsa_host_core::addon_disk::{self, AddonDisk, Format};
            let dir = image.initrd.as_deref().and_then(Path::parent);
            let attached = AddonDisk::new(&addons, Format::Raw).attached();
            addon_disk::bootconfig(&attached).to_string()
                + &dsa_host_core::arm_translation::bootconfig_for(&*store, &user, dir, &attached)
                + &dsa_host_core::gapps::bootconfig_for(&*store, &user, &attached)
        })
    })
    .with_boot_properties({
        // The user's device identity on this computer, which Android reads once while it
        // boots. The computer is read again at each start, so a renamed one is followed.
        let identity = Arc::clone(&boot_identity);
        Box::new(move |image: &ImageFiles| {
            let built = image
                .initrd
                .as_deref()
                .and_then(Path::parent)
                .and_then(ImageBuild::from_image_dir);
            Some(identity.boot_properties(built.as_ref()))
        })
    })
    .with_direct_mapped(config.direct_mapped_images);
    if config.network {
        let (store, user) = (Arc::clone(&store), user.to_string());
        launcher = launcher.with_networking(
            Box::new(move || {
                store
                    .get(&user)
                    .map(|settings| settings.advanced_networking)
                    .unwrap_or(false)
            }),
            network_status,
        );
    }
    if let Some(command) = config.window_manager.clone().filter(|c| !c.is_empty()) {
        let mut keeper = WindowManagerKeeper::new(
            crate::window_manager::CommandWindowManager::new(account.clone(), command),
        );
        launcher = launcher.with_prerequisite(Box::new(move || keeper.check(Instant::now())));
    }
    let boot_signs = Arc::new(Mutex::new(BootSigns::default()));
    let launcher = launcher.with_signs(Arc::clone(&boot_signs));

    let hub = |port: u32| VsockHub::shared(port).map_err(|e| format!("vsock port {port}: {e}"));
    let listener = hub(config.control_port)?.listener(cid, false);
    let log_listener = hub(config.log_port)?.listener(cid, false);
    let share_listener = hub(dsa_host_core::file_share::SHARE_PORT)?.listener(cid, true);

    let session_config = config.session_config();
    let handshake = ControlHandshake::new(build, session_config.connect_timeout);
    let session = Session::new(launcher, Box::new(listener), handshake, session_config);
    // The computer's name becomes the guest's device name.
    session.set_device_name_source(Arc::new(move || boot_identity.device_name()));
    // Developer mode's bridge reaches the guest's adbd on its vsock port.
    session.set_adb_connect(Arc::new(|vm_id: &str| {
        connect_stream(
            vm_cid(vm_id)?,
            dsa_host_core::adb_bridge::GUEST_ADB_PORT,
            Duration::from_secs(5),
        )
    }));
    // Developer mode's port is attached to the user's own adb server, and advertised over
    // mDNS on the loopback address, so adb finds it without `adb connect`.
    session.set_adb_server(crate::adb_discovery::adb_server_of(uid));
    session.set_adb_advertise(crate::adb_discovery::advertiser());
    session.serve_log_port(Box::new(log_listener), build);
    // The user's folders, served with the user's own rights.
    let folders = crate::devices::file_share::UserFolders::for_uid(uid)
        .map_err(|e| format!("the folders of {user}: {e}"))?;
    session.serve_file_shares(Box::new(share_listener), Arc::new(folders));
    // App data backups in the user's own folder (once they turn backup on), the passkey
    // check, and KeyMint's root key, sealed by the TPM first.
    crate::host_services::serve(&session, &account, cid, build, user, &config.settings_dir())?;
    // The guest's sound, cameras and sensors, passed on to the user's own device lender.
    for port in crate::devices::relay::RELAYED_PORTS {
        let listener = hub(port)?.listener(cid, true);
        if let Err(error) = crate::devices::relay::relay(listener, port, uid, user.to_string()) {
            eprintln!("{user}: vsock port {port}: cannot start its relay: {error}");
        }
    }
    // Android's shared storage, which the file manager opens through a loopback port.
    session.set_android_files_connect(Arc::new(|vm_id: &str| {
        connect_stream(
            vm_cid(vm_id)?,
            dsa_host_core::file_share::ANDROID_FILES_PORT,
            Duration::from_secs(5),
        )
    }));
    let line_user = user.to_string();
    session.set_clock_trace(Some(ClockTrace::new(
        trace_on,
        Arc::new(MonotonicClock),
        Arc::new(move |line: &str| eprintln!("{line_user}: {line}")),
    )));
    // An app's root request that needs the administrator is asked through polkit, in the
    // user's desktop session, by the levels in the user's settings; in a boot check that
    // names a private folder approver, it is asked through that folder instead.
    let prompt: Option<Arc<dyn dsa_host_core::root_approval::ApprovalPrompt>> = match config
        .test_root_approver()
        .map_err(|e| e.to_string())?
    {
        Some(folder) => Some(Arc::new(folder)),
        None => match crate::root_approval::SystemBusAuthority::connect() {
            Ok(authority) => Some(Arc::new(
                crate::root_approval::PolkitPrompt::new(authority, uid).with_home(&account.home),
            )),
            Err(why) => {
                eprintln!(
                    "{user}: root: polkit cannot be reached, so every root request is refused: {why}"
                );
                None
            }
        },
    };
    if let Some(prompt) = prompt {
        let log_user = user.to_string();
        session.set_root_approval(Arc::new(RootApproval::new(
            user,
            Arc::clone(&store),
            prompt,
            Arc::new(move |line: &str| eprintln!("{log_user}: {line}")),
        )));
    }
    let session = Arc::new(session);
    {
        let watched = Arc::downgrade(&session);
        if let Err(error) = thread::Builder::new()
            .name("dsa-boot-signs".into())
            .spawn(move || watch_boot_signs(watched, boot_signs))
        {
            eprintln!("{user}: guest log: cannot read Android's boot for its problems: {error}");
        }
    }
    if let Some(power) = power {
        power.attach(&session);
    }
    // The guest's display refreshes as fast as the user's fastest monitor, which their window
    // manager records; the power saver profile, which comes with the power, caps it at 60 Hz.
    let (watched, refresh_user) = (Arc::downgrade(&session), user.to_string());
    if let Err(error) = thread::Builder::new()
        .name("dsa-monitor-refresh".into())
        .spawn(move || {
            dsa_host_core::guest_refresh::follow_monitors(
                watched,
                || {
                    UserFiles::of(&host_paths(), &refresh_user)
                        .ok()
                        .and_then(|files| crate::monitor_refresh::read_record(&files.run_dir))
                },
                dsa_host_core::guest_refresh::MONITOR_POLL,
            )
        })
    {
        eprintln!("{user}: display: cannot follow the monitors' refresh rate: {error}");
    }
    // The guest's power HAL: its hints set how hard Linux runs the guest's processors.
    let hints = session.watch_power_hints();
    let (watched, hint_user) = (Arc::downgrade(&session), user.to_string());
    if let Err(error) = thread::Builder::new()
        .name("dsa-guest-power".into())
        .spawn(move || crate::performance::follow(hints, watched, hint_user))
    {
        eprintln!("{user}: power: cannot follow Android's power hints: {error}");
    }
    Ok(match updates {
        None => session as Arc<dyn UserAndroid>,
        Some(updates) => {
            let android = Arc::new(UpdatingAndroid {
                session,
                store: Arc::clone(&updates.store),
                notices: Arc::clone(&updates.notices),
                boot_timeout: updates.boot_timeout,
                trial_running: Arc::clone(&updates.trial_running),
                rollback: Default::default(),
                image_restart: Default::default(),
            });
            // An image downloaded while the user's Android runs is applied by restarting it.
            follow_waiting_image(&android, WAITING_IMAGE_CHECK);
            android as Arc<dyn UserAndroid>
        }
    })
}

/// Opens the update store when the configuration names one.
fn open_updates(config: &HostConfig, build: &str) -> Result<Option<Updates>, String> {
    let (Some(root), Some(first)) = (&config.images_dir, &config.first_image) else {
        return Ok(None);
    };
    let store = ImageStore::open_as(root, first, build).map_err(|e| e.to_string())?;
    match store.recover(first) {
        Ok(Recovery::Usable) => {}
        Ok(Recovery::Moved { broken, current }) => eprintln!(
            "{build}: the image {} has missing or broken files; using {} instead",
            broken.version, current.version
        ),
        Ok(Recovery::NothingBetter { broken }) => {
            if store.has_image(&broken.version) {
                eprintln!(
                    "{build}: the image {} has missing or broken files and no other image is \
                     installed",
                    broken.version
                );
            }
        }
        Err(why) => eprintln!("{build}: checking the current image: {why}"),
    }
    let boot_timeout = config
        .update
        .as_ref()
        .map(|u| u.boot_timeout_secs)
        .unwrap_or(300);
    Ok(Some(Updates {
        store: Arc::new(store),
        notices: Arc::new(UpdateNotices::default()),
        boot_timeout: Duration::from_secs(boot_timeout),
        trial_running: Arc::new(AtomicBool::new(false)),
        check_now: Arc::new(CheckNow::default()),
    }))
}

/// Checks the release list a minute after the service starts and then every
/// `check_interval_hours`, until `stop` is set. Without an image to boot it checks at once and
/// again every few minutes until one is in place.
fn check_for_updates(
    config: HostConfig,
    updates: Updates,
    build: String,
    stop: Arc<AtomicBool>,
    store: Arc<dyn SettingsStore>,
) {
    let Some(settings) = config.update.clone() else {
        return;
    };
    let keys = match settings.keys() {
        Ok(keys) => keys,
        Err(error) => return eprintln!("{build}: not checking for updates: {error}"),
    };
    let interval = Duration::from_secs(settings.check_interval_hours.max(1) * 3600);
    let no_image = || {
        updates
            .store
            .state()
            .map(|state| !updates.store.has_image(&state.current.version))
            .unwrap_or(false)
    };
    let mut next = if no_image() {
        Instant::now()
    } else {
        Instant::now() + Duration::from_secs(60)
    };
    let fetch = crate::fetch::CurlFetch;
    while !stop.load(Ordering::Relaxed) {
        next = next_image_check(next, Instant::now(), !no_image(), FIRST_IMAGE_RETRY);
        // A user who chose another Android version waits for it now, not at the next turn.
        if updates.check_now.take() {
            next = Instant::now();
        }
        if Instant::now() < next {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        let waiting_for_first = no_image();
        if !waiting_for_first && !updates.store.switch_pending() && !store.machine().image_updates {
            next = Instant::now() + SETTINGS_RECHECK;
            continue;
        }
        let mut shown = None;
        let mut announce = |download: &Download| {
            let percent = (download.done as u128 * 100)
                .checked_div(download.total as u128)
                .unwrap_or(100);
            if shown == Some(percent) {
                return;
            }
            shown = Some(percent);
            let notice = download.notice();
            if download.done == 0 {
                eprintln!("{build}: {notice}");
            }
            updates.notices.set(notice);
        };
        match check_alerting(
            &updates.store,
            &fetch,
            &settings.list_url,
            &keys,
            &updates.notices,
            &mut announce,
        ) {
            Ok(Some(notice)) => {
                eprintln!("{build}: {notice}");
                updates.notices.set(notice);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("{build}: checking for updates: {error}");
                if waiting_for_first {
                    updates.notices.set(format!(
                        "the Android image could not be downloaded yet ({error}); the service \
                         tries again in {} minutes",
                        FIRST_IMAGE_RETRY.as_secs() / 60
                    ));
                }
            }
        }
        next = Instant::now()
            + if no_image() {
                FIRST_IMAGE_RETRY
            } else {
                interval
            };
    }
}

/// The host's logs that go in a user's diagnostics: the console of their Android.
pub fn host_logs_of(user: &str) -> Vec<HostFile> {
    let Ok(files) = UserFiles::of(&host_paths(), user) else {
        return Vec::new();
    };
    let mut old = files.console_log.as_os_str().to_owned();
    old.push(".old");
    vec![
        HostFile {
            name: "host/android-console.log".into(),
            path: files.console_log,
            only_user: None,
        },
        HostFile {
            name: "host/android-console.1.log".into(),
            path: PathBuf::from(old),
            only_user: None,
        },
    ]
}

/// Logs the memory of every running user's Android every few minutes until `stop` is set.
fn log_memory(androids: &dyn UserAndroids, host: &dyn HostMemory, build: &str, stop: &AtomicBool) {
    let mut next = Instant::now() + LOG_INTERVAL;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        if Instant::now() < next {
            continue;
        }
        next = Instant::now() + LOG_INTERVAL;
        for line in summary_lines(androids, host) {
            eprintln!("{build}: {line}");
        }
    }
}

/// The network status of each user's running VM.
type Networks = Arc<Mutex<std::collections::HashMap<String, SharedNetworkStatus>>>;

fn network_of(networks: &Networks, user: &str) -> SharedNetworkStatus {
    Arc::clone(
        networks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(user.to_string())
            .or_default(),
    )
}

/// Serves the local API until `stop` is set, then stops every user's VM.
pub fn serve(config: HostConfig, build: &str, stop: Arc<AtomicBool>) -> Result<(), String> {
    if let Some(folder) = config.socket.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("creating {}: {e}", folder.display()))?;
    }
    // Every user may connect; the service acts on each caller's own VM.
    let listener = UnixLocalListener::bind(&config.socket, 0o666)
        .map_err(|e| format!("opening the socket {}: {e}", config.socket.display()))?;
    eprintln!(
        "{build}: serving the local API on {}",
        config.socket.display()
    );
    if !crate::users::is_root() {
        eprintln!(
            "{build}: running without root: only this user's Android can run, and advanced \
             networking cannot be set up"
        );
    }
    let grace = Duration::from_secs(config.stop_timeout_secs + 15);
    let store = service_settings_store(&config, build);
    let updates = match open_updates(&config, build) {
        Ok(updates) => updates,
        Err(error) => {
            eprintln!("{build}: image updates disabled: {error}");
            None
        }
    };
    if let Some(updates) = &updates {
        let (config, updates, worker_build, stop, store) = (
            config.clone(),
            updates.clone(),
            build.to_string(),
            Arc::clone(&stop),
            Arc::clone(&store),
        );
        if let Err(error) = thread::Builder::new()
            .name("dsa-updates".into())
            .spawn(move || check_for_updates(config, updates, worker_build, stop, store))
        {
            eprintln!("{build}: starting the update check failed: {error}");
        }
    }
    let facts = ServiceFacts {
        version: crate::VERSION.into(),
        log_dir: None,
        // The system's package manager updates DSA itself; `dsa update` says so.
        program_updates_available: false,
        // The release channel chooses which of DSA's package repositories the entries point at.
        update_channel_available: true,
        image_updates_available: updates.is_some() && config.update.is_some(),
        android_version: config.first_image.as_ref().map(|i| i.version.clone()),
    };
    let android_version: Option<dsa_host_core::host_api::AndroidVersionSource> =
        updates.as_ref().map(|updates| {
            let store = Arc::clone(&updates.store);
            Arc::new(move || store.state().ok().map(|state| state.current.version))
                as dsa_host_core::host_api::AndroidVersionSource
        });
    // Every user's Android follows the computer's power and sleep.
    let power = Arc::new(PowerHub::new());
    {
        let (power, stop) = (Arc::clone(&power), Arc::clone(&stop));
        if let Err(error) = thread::Builder::new()
            .name("dsa-power".into())
            .spawn(move || crate::devices::power::follow(power, stop))
        {
            eprintln!("{build}: following the computer's power failed: {error}");
        }
    }
    // Kept for as long as the service runs.
    let _sleep_watch = match crate::devices::power::watch_sleep(Arc::clone(&power)) {
        Ok(watch) => Some(watch),
        Err(why) => {
            eprintln!("{build}: Android will not hear when the computer sleeps: {why}");
            None
        }
    };
    let networks: Networks = Arc::default();
    let identity_updates = updates.clone();
    let translator_updates = updates.clone();
    let reset_updates = updates.clone();
    let factory_config = config.clone();
    let factory_build = build.to_string();
    let factory_store = Arc::clone(&store);
    let factory_networks = Arc::clone(&networks);
    let factory_updates = updates.clone();
    let androids = Arc::new(PerUser::new(move |user: &str| {
        let android = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            create_user_android(
                &factory_config,
                &factory_build,
                user,
                factory_updates.as_ref(),
                Some(power.as_ref()),
                network_of(&factory_networks, user),
            )
        })) {
            Ok(result) => result,
            Err(_) => Err(format!("setting up Android for {user} failed unexpectedly")),
        };
        match &android {
            Ok(android) => {
                let settings = factory_store.get(user).ok();
                android.freeze_when_idle(settings.as_ref().is_some_and(|s| s.freeze));
                android.share_clipboard(settings.as_ref().is_none_or(|s| s.clipboard));
                android.links_in_android(settings.as_ref().is_some_and(|s| s.links_in_android));
                android.image_auto_restart(settings.as_ref().is_none_or(|s| s.image_auto_restart));
                android
                    .trust_host_certificates(settings.as_ref().is_none_or(|s| s.host_certificates));
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
                        eprintln!("{user}: developer mode: {why}");
                    }
                }
                // Root elevation: the computer's switch, and the level of each of their apps.
                dsa_host_core::root_elevation::apply(
                    android.as_ref(),
                    factory_store.as_ref(),
                    user,
                );
            }
            Err(why) => eprintln!("{user}: no Android: {why}"),
        }
        android
    }));
    let host_memory: Arc<dyn HostMemory> = Arc::new(crate::memory::ProcHostMemory);
    let reset_config = config.clone();
    let data_reset = TemplateDataDisk::new(
        Box::new(|user: &str| -> Result<PathBuf, String> {
            Ok(UserFiles::of(&host_paths(), user)?.data_disk)
        }),
        Box::new(move |disk: &Path| -> Result<(), String> {
            use std::os::unix::fs::MetadataExt;
            // The disk's folder is the user's own.
            let folder = disk.parent().ok_or("the disk has no folder")?;
            let owner = std::fs::metadata(folder)
                .map_err(|e| format!("reading {}: {e}", folder.display()))?
                .uid();
            let account = Account::of_uid(owner)?;
            let (dir, version) = current_image(&reset_config, reset_updates.as_ref())?;
            let image = image_files(&dir, reset_config.kernel.as_deref(), "")?;
            make_disk(
                &account,
                disk,
                image.disk_template.as_deref(),
                reset_config.data_disk_size_gb,
            )?;
            match version {
                Some(version) => {
                    account.write_private(&crate::image::version_file(disk), version.as_bytes())
                }
                None => Ok(()),
            }
        }),
    )
    .with_beside("version");
    let identity_config = config.clone();
    let translator_config = config.clone();
    let mut service = HostApiService::new(androids.clone(), build)
        .with_data_reset(Arc::new(data_reset))
        .with_program_updates(Arc::new(crate::package_updates::PackageManagerUpdates))
        .with_settings(Arc::clone(&store), facts)
        .with_host_memory(Arc::clone(&host_memory))
        .with_network({
            let networks = Arc::clone(&networks);
            Arc::new(move |user: &str, _: &dsa_host_core::settings::Settings| {
                Some(
                    network_of(&networks, user)
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .unwrap_or_else(NetworkStatus::off),
                )
            })
        })
        .with_widget_placements(Arc::new(dsa_host_core::widgets::FileWidgetPlacements::new(
            config.settings_dir(),
        )))
        .with_host_logs(Arc::new(host_logs_of))
        .with_device_identities(
            Arc::new(FileDeviceIdentities::new(config.settings_dir())),
            Arc::new(move |_: &str| {
                let (dir, _) = current_image(&identity_config, identity_updates.as_ref()).ok()?;
                ImageBuild::from_image_dir(&dir)
            }),
        )
        // The defaults the settings show are what this computer says it is now.
        .with_computer_identity(Arc::new(crate::computer_identity::read))
        // The default ARM translator of the image the next start boots: only an x86_64 image
        // has one.
        .with_default_arm_translation(Arc::new(move |_: &str| {
            let (dir, _) = current_image(&translator_config, translator_updates.as_ref()).ok()?;
            dsa_host_core::arm_translation::default_package(&dir)
        }));
    if let Some(source) = android_version {
        service = service.with_android_version(source);
    }
    // Google Play, for a user who asks: downloaded from LiteGapps, or the mirror the
    // configuration names, and made ready in the service's own folder.
    let mut gapps = dsa_host_core::gapps::Source::new(
        Arc::new(crate::fetch::CurlFetch),
        config.settings_dir().with_file_name("gapps"),
    );
    if let Some(project) = config.gapps_project.as_deref().filter(|p| !p.is_empty()) {
        gapps = gapps.with_project(project);
    }
    service = service.with_gapps(gapps);
    // Each user's add-ons, on the add-on disk in their data folder, which their VM attaches
    // read only.
    service = service.with_addon_disks(Arc::new(|user: &str| {
        let files = UserFiles::of(&host_paths(), user)?;
        Ok(dsa_host_core::addon_disk::AddonDisk::new(
            files.addon_disk,
            dsa_host_core::addon_disk::Format::Raw,
        ))
    }));
    if let Some(updates) = &updates {
        service = service.with_android_versions(Arc::new(StoreVersions {
            store: Arc::clone(&updates.store),
            notices: Arc::clone(&updates.notices),
            check_now: Arc::clone(&updates.check_now),
            shared_by_users: true,
        }));
    }
    {
        let androids = androids.clone();
        let (worker_build, stop) = (build.to_string(), Arc::clone(&stop));
        if let Err(error) = thread::Builder::new()
            .name("dsa-memory-log".into())
            .spawn(move || log_memory(&*androids, &*host_memory, &worker_build, &stop))
        {
            eprintln!("{build}: starting the memory log failed: {error}");
        }
    }
    let service = Arc::new(service);
    service.serve(Box::new(listener), stop);
    eprintln!("{build}: stopping every user's Android");
    let not_stopped = androids.stop_all(grace);
    if not_stopped.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "these users' Android VMs did not stop: {}",
            not_stopped
                .iter()
                .map(|(user, state)| format!("{user} ({state})"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr2_the_vm_starts_small_and_may_grow_to_the_computer_less_the_hosts_share() {
        let (start, max) = memory_sizing(0, Some(32 * 1024));
        assert_eq!(start, DEFAULT_MEMORY_MB);
        let max = max.unwrap();
        assert!(max > start && max < 32 * 1024);
        // A user's limit caps both.
        assert_eq!(memory_sizing(2048, Some(32 * 1024)), (2048, None));
        assert_eq!(
            memory_sizing(8192, Some(32 * 1024)),
            (DEFAULT_MEMORY_MB, Some(8192))
        );
        // Without the computer's size the VM keeps its start.
        assert_eq!(memory_sizing(0, None), (DEFAULT_MEMORY_MB, None));
    }

    #[test]
    fn fr19_each_users_android_files_are_in_their_own_data_folder() {
        let paths = UnixHostPaths::new(
            Layout::Linux,
            "/var/lib/dsa",
            Box::new(|_: &str| Ok(PathBuf::from("/home/ana"))),
        );
        let files = UserFiles::of(&paths, "uid:1000").unwrap();
        assert_eq!(
            files.data_disk,
            PathBuf::from("/home/ana/.local/share/dsa/data.img")
        );
        // A disk an earlier DSA made as qcow2 stays the user's disk.
        let root = std::env::temp_dir().join(format!("dsa-user-disk-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(user_disk(&root), root.join("data.img"));
        std::fs::write(root.join("data.qcow2"), b"QFI").unwrap();
        assert_eq!(user_disk(&root), root.join("data.qcow2"));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            files.console_log,
            PathBuf::from("/home/ana/.local/share/dsa/logs/android-console.log")
        );
        assert_eq!(
            files.run_dir,
            PathBuf::from("/home/ana/.local/share/dsa/tmp")
        );
    }

    #[test]
    fn fr5_the_clock_is_paired_only_for_a_guest_booted_with_the_input_trace() {
        assert!(guest_traces_input(
            "androidboot.hardware.dsa_input_trace=1 a=b"
        ));
        assert!(!guest_traces_input(
            "androidboot.hardware.dsa_input_trace=0"
        ));
    }

    #[test]
    fn fr35_the_test_approver_uses_real_settings_and_clears_grace_on_a_new_boot() {
        use dsa_host_core::root_approval::{
            ApprovalPrompt, FolderApprover, RootApproval, ANSWER_FILE, REQUESTS_FILE,
        };
        use dsa_host_core::settings::MemorySettings;
        use dsa_protocol::control::SuRequest;

        let dir = std::env::temp_dir().join(format!("dsa-test-approver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ANSWER_FILE), "allow\n").unwrap();

        let store: Arc<dyn SettingsStore> = Arc::new(MemorySettings::default());
        let mut machine = store.machine();
        machine.root_elevation = true;
        store.set_machine(machine).unwrap();
        store.set_root_app("uid:0", "com.termux", "ask").unwrap();

        let prompt: Arc<dyn ApprovalPrompt> = Arc::new(FolderApprover::new(&dir));
        let approval =
            RootApproval::new("uid:0", Arc::clone(&store), prompt, Arc::new(|_: &str| {}));

        let call = |request_id: u64| {
            approval.decide(
                &SuRequest {
                    request_id,
                    package: "com.termux".into(),
                    uid: 10_142,
                    command: vec!["id".into()],
                    interactive: false,
                },
                None,
            )
        };
        assert!(call(1).approved);
        assert!(call(2).approved);
        assert_eq!(
            std::fs::read_to_string(dir.join(REQUESTS_FILE))
                .unwrap()
                .lines()
                .count(),
            1,
            "the second su call within grace does not prompt again"
        );

        approval.restart();
        assert!(call(3).approved);
        assert_eq!(
            std::fs::read_to_string(dir.join(REQUESTS_FILE))
                .unwrap()
                .lines()
                .count(),
            2,
            "restarting clears the grace and prompts again"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
