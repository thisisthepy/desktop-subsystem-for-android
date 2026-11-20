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
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::boot::{BootPlanner, MemoryInputs};
use dsa_host_core::boot_signs::{watch_boot_signs, BootSigns};
use dsa_host_core::clock_trace::{ClockTrace, HostClock};
use dsa_host_core::data_reset::TemplateDataDisk;
use dsa_host_core::device_identity::{BootIdentity, FileDeviceIdentities, ImageBuild};
use dsa_host_core::diagnostics::HostFile;
use dsa_host_core::direct_mapped_disk::{DirectMappedImages, Store};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{
    HostApiService, NetworkStatus, PerUser, ServiceFacts, UserAndroid, UserAndroids,
};
use dsa_host_core::host_config::{service_file_settings, updates_config, HostConfigFile};
use dsa_host_core::host_facts::HostFacts;
use dsa_host_core::host_paths::HostPaths;
use dsa_host_core::input_trace::guest_traces_input;
use dsa_host_core::memory::GrowthCeiling;
use dsa_host_core::memory::{log_memory, HostMemory};
use dsa_host_core::power::PowerHub;
use dsa_host_core::root_approval::RootApproval;
use dsa_host_core::session::Session;
use dsa_host_core::settings::SettingsStore;
use dsa_host_core::update::current::{current_image, CurrentImage};
use dsa_host_core::update::service::{
    follow_waiting_image, open_updates_or_disable, run_update_checks, StoreVersions, UpdateCheck,
    Updates, UpdatingAndroid, WAITING_IMAGE_CHECK,
};
use dsa_host_core::window_manager::WindowManagerKeeper;
use dsa_host_unix::paths::{Layout, UnixHostPaths};
use dsa_host_unix::socket::UnixLocalListener;

use crate::config::HostConfig;
use crate::crosvm::guest_cid;
use crate::image::{ImageFiles, UserDisks};
use crate::launcher::{BootPlan, LauncherSetup, LinuxVmLauncher, SharedNetworkStatus};
use crate::users::{uid_of, Account};
use crate::vsock::{connect_stream, VsockHub};

/// What this computer says about itself, from `/proc`.
pub struct ProcFacts;

impl HostFacts for ProcFacts {
    fn physical_memory_kb(&self) -> Option<u64> {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        crate::devices::power::parse_meminfo(&text).map(|(total_kb, _)| total_kb)
    }
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
    dsa_host_core::host_config::settings_store(config.settings_dir(), None, None)
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
    let files = service_file_settings(config.settings_dir(), first, None, build);
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

/// Starts the maker of direct-mapped system disks when the host's direct-mapped mode is on: it
/// keeps the uncompressed copy of the current image's system disk in `direct-mapped` in the
/// machine's folder, made in the background after the image was installed or downloaded and
/// checked. The folder is the service's (root's when it runs as root) and its files are
/// readable by every user, whose crosvm maps them read only.
fn start_direct_mapped(
    config: &HostConfig,
    updates: Option<&Updates>,
    build: &str,
) -> Option<Arc<DirectMappedImages>> {
    if !config.direct_mapped_images {
        return None;
    }
    let folder = config.machine_dir.join("direct-mapped");
    if let Err(why) = std::fs::create_dir_all(&folder) {
        eprintln!(
            "{build}: direct-mapped: creating {}: {why}",
            folder.display()
        );
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755));
    }
    let (config, updates, log_build) = (config.clone(), updates.cloned(), build.to_string());
    Some(DirectMappedImages::start(
        Store::raw(folder),
        Box::new(move || {
            let CurrentImage { dir, version, .. } = current_image(
                updates.as_ref().map(|u| u.store.as_ref()),
                config.image_dir.as_deref(),
            )
            .ok()?;
            let version = version
                .or_else(|| dsa_host_core::direct_mapped_disk::worker::image_version(&dir))?;
            Some((version, dir))
        }),
        Box::new(lower_this_thread),
        Arc::new(move |line: &str| eprintln!("{log_build}: {line}")),
    ))
}

/// Checks the raw copy of the current image's system disk byte by byte, in the background at
/// the lowest priority: one that changed while the service was not running is removed, and the
/// next boot makes it again from the image's checked VHDX.
fn check_raw_system_disk(config: &HostConfig, updates: Option<&Updates>, build: &str) {
    let label = build.to_string();
    let (config, updates, build) = (config.clone(), updates.cloned(), build.to_string());
    let spawned = thread::Builder::new()
        .name("dsa-check-system-disk".into())
        .spawn(move || {
            if let Err(why) = lower_this_thread() {
                eprintln!("{build}: checking the raw system disk at normal priority: {why}");
            }
            let Ok(CurrentImage { dir, .. }) = current_image(
                updates.as_ref().map(|u| u.store.as_ref()),
                config.image_dir.as_deref(),
            ) else {
                return;
            };
            dsa_host_core::disk::raw_copy::verify_raw_copy_logged(
                &dir.join("system.img"),
                &dir.join("system.vhdx"),
                &dir,
                &build,
            );
        });
    if let Err(why) = spawned {
        eprintln!("{label}: the raw system disk is not checked: {why}");
    }
}

/// Takes the calling thread to the lowest CPU priority (nice 19) and the idle I/O class, for
/// as long as it runs.
fn lower_this_thread() -> Result<(), String> {
    // SAFETY: plain system calls on the calling thread (0 names it for both).
    unsafe {
        if libc::setpriority(libc::PRIO_PROCESS, 0, 19) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // ioprio_set(IOPRIO_WHO_PROCESS, this thread, IOPRIO_CLASS_IDLE << 13).
        let (who, this, idle): (libc::c_long, libc::c_long, libc::c_long) = (1, 0, 3 << 13);
        if libc::syscall(libc::SYS_ioprio_set, who, this, idle) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    Ok(())
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
    direct_mapped: Option<&Arc<DirectMappedImages>>,
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
    // The most this start's VM may grow to, in MiB, which the boot tells the guest so that apps
    // see it as the device's memory. Set by each start's plan, read by its bootconfig.
    let apps_memory_mb: Arc<Mutex<Option<u64>>> = Arc::default();
    let plan = {
        let apps_memory = Arc::clone(&apps_memory_mb);
        let grow_memory = config.grow_memory;
        let (disk, run_dir, traced) = (
            files.data_disk.clone(),
            files.run_dir.clone(),
            Arc::clone(&trace_on),
        );
        let planner = BootPlanner {
            store: updates.map(|u| Arc::clone(&u.store)),
            configured_dir: config.image_dir.clone(),
            rules: crate::image::image_rules(config.kernel.as_deref(), &config.kernel_cmdline),
            check: None,
            data_disk: files.data_disk.clone(),
            data_disk_size_gb: config.data_disk_size_gb,
            image_cache: None,
            backend: UserDisks(account.clone()),
            memory: MemoryInputs {
                fixed_mb: config.memory_mb,
                settings: Arc::clone(&store),
                user: user.to_string(),
                ceiling: GrowthCeiling::HostLessReserve,
                facts: Arc::new(ProcFacts),
                hypervisor_max_mb: None,
            },
        };
        Box::new(move || -> Result<BootPlan, String> {
            crate::image::adopt_legacy_version_record(&disk);
            let boot = planner.plan()?;
            let mut image = boot.files;
            // Every app starts at the density of the user's main monitor, which their window
            // manager records, so the windows that open there ask for no other.
            image.kernel_cmdline =
                crate::monitor_density::with_boot_density(&image.kernel_cmdline, &run_dir);
            traced.store(guest_traces_input(&image.kernel_cmdline), Ordering::Relaxed);
            let grows_to = if grow_memory {
                boot.memory.grows_to_mb()
            } else {
                u64::from(boot.memory.start_mb)
            };
            *apps_memory.lock().unwrap_or_else(|e| e.into_inner()) = Some(grows_to);
            Ok(BootPlan {
                files: image,
                image_version: boot.image_version,
                memory_mb: boot.memory.start_mb,
                memory_max_mb: boot
                    .memory
                    .max_mb
                    .map(|mb| u32::try_from(mb).unwrap_or(u32::MAX)),
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
        let apps_memory = Arc::clone(&apps_memory_mb);
        Box::new(move |image: &ImageFiles| {
            use dsa_host_core::addon_disk::{AddonDisk, Format};
            let dir = image.initrd.as_deref().and_then(Path::parent);
            let attached = AddonDisk::new(&addons, Format::Raw).attached();
            // The memory the VM may grow to, which apps see as the device's memory; set by
            // this start's plan.
            dsa_host_core::bootconfig::for_boot(
                &*store,
                &user,
                dir,
                &attached,
                &ProcFacts,
                *apps_memory.lock().unwrap_or_else(|e| e.into_inner()),
            )
        })
    })
    .with_boot_properties({
        // The user's device identity on this computer, which Android reads once while it
        // boots. The computer is read again at each start, so a renamed one is followed.
        let identity = Arc::clone(&boot_identity);
        Box::new(move |image: &ImageFiles| {
            Some(identity.boot_properties_of_initrd(image.initrd.as_deref()))
        })
    })
    .with_direct_mapped(direct_mapped.cloned());
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
    // Polkit's dialog has longer to be answered while a screen reader runs in the user's
    // session, as hearing it read out takes longer than reading it; that is asked as each
    // prompt opens.
    let mut screen_reader: Option<dsa_host_core::root_approval::ScreenReader> = None;
    let prompt: Option<Arc<dyn dsa_host_core::root_approval::ApprovalPrompt>> = match config
        .test_root_approver()
        .map_err(|e| e.to_string())?
    {
        Some(folder) => Some(Arc::new(folder)),
        None => match crate::root_approval::SystemBusAuthority::connect() {
            Ok(authority) => {
                let reader = account.clone();
                screen_reader = Some(Arc::new(move || crate::screen_reader::running_for(&reader)));
                Some(Arc::new(
                    crate::root_approval::PolkitPrompt::new(authority, uid)
                        .with_home(&account.home),
                ))
            }
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
        let mut approval = RootApproval::new(
            user,
            Arc::clone(&store),
            prompt,
            Arc::new(move |line: &str| eprintln!("{log_user}: {line}")),
        );
        if let Some(screen_reader) = screen_reader {
            approval = approval.with_screen_reader(screen_reader);
        }
        session.set_root_approval(Arc::new(approval));
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

/// The host's logs that go in a user's diagnostics: the console of their Android.
pub fn host_logs_of(user: &str) -> Vec<HostFile> {
    let Ok(files) = UserFiles::of(&host_paths(), user) else {
        return Vec::new();
    };
    dsa_host_core::console_log::diagnostic_files(&files.console_log)
        .into_iter()
        .map(|(name, path)| HostFile {
            name: name.into(),
            path,
            only_user: None,
        })
        .collect()
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
    // A store that cannot be opened turns image updates off; Android still starts from the
    // image it has, and the reason is shown with the Android version.
    let preset_dir = installer_preset_dir();
    let (updates, updates_disabled) = match open_updates_or_disable(
        dsa_host_core::update::service::UpdatesConfig {
            preset_dir: preset_dir.as_deref(),
            ..updates_config(&config)
        },
        build,
    ) {
        Ok(updates) => (updates, None),
        Err(disabled) => (None, Some(disabled)),
    };
    let direct_mapped = start_direct_mapped(&config, updates.as_ref(), build);
    check_raw_system_disk(&config, updates.as_ref(), build);
    if let (Some(updates), Some(settings)) = (&updates, config.update.clone()) {
        let check = UpdateCheck {
            settings,
            updates: updates.clone(),
            store: Arc::clone(&store),
            build: build.to_string(),
            // A new image was downloaded and checked: its direct-mapped system disk is made
            // now, in the background, before the next start of Android needs it.
            on_new_image: direct_mapped
                .clone()
                .map(|images| Box::new(move || images.kick()) as Box<dyn Fn() + Send>),
        };
        let stop = Arc::clone(&stop);
        if let Err(error) = thread::Builder::new()
            .name("dsa-updates".into())
            .spawn(move || run_update_checks(check, &crate::fetch::CurlFetch, stop))
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
                direct_mapped.as_ref(),
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
            dsa_host_core::boot::make_fresh_disk(
                &UserDisks(account),
                &crate::image::image_rules(reset_config.kernel.as_deref(), ""),
                (
                    reset_updates.as_ref().map(|u| u.store.as_ref()),
                    reset_config.image_dir.as_deref(),
                ),
                None,
                disk,
                reset_config.data_disk_size_gb,
            )
        }),
    )
    .with_beside("image-version")
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
                let CurrentImage { dir, .. } = current_image(
                    identity_updates.as_ref().map(|u| u.store.as_ref()),
                    identity_config.image_dir.as_deref(),
                )
                .ok()?;
                ImageBuild::from_image_dir(&dir)
            }),
        )
        // The defaults the settings show are what this computer says it is now.
        .with_computer_identity(Arc::new(crate::computer_identity::read))
        // The default ARM translator of the image the next start boots: only an x86_64 image
        // has one.
        .with_default_arm_translation(Arc::new(move |_: &str| {
            let CurrentImage { dir, .. } = current_image(
                translator_updates.as_ref().map(|u| u.store.as_ref()),
                translator_config.image_dir.as_deref(),
            )
            .ok()?;
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
    } else if let Some(disabled) = updates_disabled {
        service = service.with_android_versions(Arc::new(disabled));
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

/// Where this installation's preset file would be. A preset package (such as a16) installs installer-preset.json in share/dsa beside the
/// program's bin folder.
fn installer_preset_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe().ok().and_then(|exe| {
        exe.parent()?
            .parent()
            .map(|prefix| prefix.join("share").join("dsa"))
    })
}
