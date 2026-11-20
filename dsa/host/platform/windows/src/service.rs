//! The DSA service: the local API on a named pipe, and one Android VM per user behind it.
//!
//! `dsa-hostd` runs this in the foreground (`serve`) or as a Windows service (`service`).
//! Each caller's SID, as Windows reports it for the pipe connection, picks the VM: the
//! first request of a user creates their data disk and their session, and every later one
//! reaches the same session.
//!
//! With `images_dir`, every VM boots the image the update store calls current, and with
//! `update` a background thread checks the signed release list and downloads new images.
//! The next start of a user's Android boots a new image on trial and goes back to the
//! previous one when it does not finish booting; users are told through the local API.
//!
//! With `update.program_list_url`, another thread checks the signed program list and
//! downloads a newer setup file, and runs it once no user's Android is running, or at
//! once on `dsa update`. Setup stops this service and starts the new one, which tells the
//! users how the update went.
//!
//! Android cannot boot without the renderer in the user's window manager, so with
//! `keep_window_manager` each user's Android waits for it: the service starts it in the user's
//! session when their Android starts, and again if it dies (`window_manager`).
//!
//! The settings users change from the settings app live in the service's own folder
//! (`settings_dir`). Turning either kind of update off there stops the checks while the
//! service runs; the render GPU is for each user's window manager, which asks for it.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::device_identity::{BootIdentity, FileDeviceIdentities, ImageBuild};
use dsa_host_core::diagnostics::HostFile;
use dsa_host_core::direct_mapped_disk::DirectMappedImages;
use dsa_host_core::guest_display::{display_or_smallest, with_display_density, with_display_size};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{HostApiService, PerUser, ServiceFacts, UserAndroid, UserAndroids};
use dsa_host_core::host_config::{updates_config, HostConfigFile};
use dsa_host_core::input_trace::guest_traces_input;
use dsa_host_core::memory::GrowthCeiling;
use dsa_host_core::memory::{log_memory, HostMemory};
use dsa_host_core::power::PowerHub;
use dsa_host_core::session::Session;
use dsa_host_core::settings::SettingsStore;
use dsa_host_core::update::current::current_image;
use dsa_host_core::update::program_service::{
    ProgramStatus, ProgramUpdates, Timing, WithProgramStatus,
};
pub use dsa_host_core::update::service::Updates;
use dsa_host_core::update::service::{
    follow_waiting_image, open_updates_or_disable, run_update_checks, StoreVersions, UpdateCheck,
    UpdatingAndroid, WAITING_IMAGE_CHECK,
};
use dsa_host_core::update::store::ImageStore;
use dsa_host_core::window_manager::WindowManagerKeeper;
use dsa_transport::Listener;

use crate::android_vm::AndroidVmLauncher;
use crate::compositor::log::PerformanceCounterClock;
use crate::guest_communication_services::register_graphics_service_guids;
use crate::guid::Guid;
use crate::host_config::{local_app_data, HostConfig};
use crate::host_memory::WindowsHostMemory;
use crate::https::{UrlFetch, WinHttpFetch};
use crate::hvsocket::HvSocketListener;
use crate::monitors::{recorded_density, recorded_display};
use crate::pipe::{NamedPipeListener, DEFAULT_PIPE_SDDL};
use crate::render_gpu::previous_render_gpu;
use crate::setup::SetupInstaller;
use crate::user_disk::{prepare_user_data_disk, prepare_user_disk_folder};
use crate::vm::AndroidVmConfig;
use crate::vm_listener::VmBoundListener;
use crate::window_manager::UserWindowManager;
use dsa_host_core::boot::{BootPlanner, MemoryInputs};
use dsa_host_core::boot_signs::{watch_boot_signs, BootSigns};
use dsa_host_core::clock_trace::ClockTrace;

/// Sizes the guest's display for the monitors the user's session recorded, and gives it the
/// main monitor's density, which every app starts at. This process runs in session 0 and
/// cannot see them itself. Without a record the display is the smallest square, so a window
/// can still be taller than the image's default display, at the density of a 100% monitor.
fn follow_monitors(vm: &mut AndroidVmConfig, sid: &str) {
    let dir = local_app_data(sid).ok();
    let display = dir.as_deref().and_then(recorded_display);
    let size = display.unwrap_or_else(|| display_or_smallest(None));
    vm.kernel_cmdline = with_display_size(&vm.kernel_cmdline, size);
    let density = match dir.as_deref() {
        Some(dir) => recorded_density(dir),
        None => dsa_host_core::guest_display::boot_density(None),
    };
    vm.kernel_cmdline = with_display_density(&vm.kernel_cmdline, density);
}

/// The host's logs that go in `sid`'s diagnostics: the service's own (without the lines about
/// other users), the console of that user's Android, and the logs of the processes in their
/// session. Never another user's.
pub fn host_logs_of(config: &HostConfig, sid: &str) -> Vec<HostFile> {
    let mut files = Vec::new();
    if let Some(log) = &config.log_file {
        files.push(HostFile {
            name: "host/dsa-hostd.log".into(),
            path: log.clone(),
            only_user: Some(sid.to_string()),
        });
    }
    let user_root = local_app_data(sid)
        .ok()
        .map(|dir| crate::paths::UserRoot::in_local_app_data(&dir));
    // A log in the user's own folder is checked as the user (and never through a link or a
    // hard link), so the service does not open what the user pointed a link at.
    let plain_in_user_root = |path: &Path| -> bool {
        let Some(root) = user_root
            .as_ref()
            .filter(|root| crate::safe_fs::is_below(path, root.dir()))
        else {
            return true;
        };
        if std::fs::symlink_metadata(root.dir()).is_err() {
            return true;
        }
        let check = || -> bool {
            if crate::safe_fs::guard_below(root.dir(), root.dir()).is_err() {
                return false;
            }
            if let Some(folder) = path.parent() {
                if std::fs::symlink_metadata(folder).is_err() {
                    return true;
                }
            }
            crate::safe_fs::guard_files(root.dir(), &[path]).is_ok()
        };
        crate::safe_fs::as_user(sid, check).unwrap_or_else(|_| check())
    };
    if let Ok(user) = config.user_vm(sid) {
        for (name, path) in dsa_host_core::console_log::diagnostic_files(&user.console_log) {
            if plain_in_user_root(&path) {
                files.push(HostFile {
                    name: name.into(),
                    path,
                    only_user: None,
                });
            }
        }
    }
    if let Some(root) = &user_root {
        for (name, path) in [
            ("host/dsa-wm.log", root.window_manager_log()),
            ("host/dsa-app-window.log", root.app_window_log()),
            (
                "host/renderer-color-buffers.txt",
                root.renderer_color_buffers(),
            ),
        ] {
            if plain_in_user_root(&path) {
                files.push(HostFile {
                    name: name.into(),
                    path,
                    only_user: None,
                });
            }
        }
    }
    files
}

/// The network of each user's VM, by SID, which the settings report.
fn user_network(sid: &str) -> crate::hcn::SharedNetwork {
    static NETWORKS: std::sync::OnceLock<
        Mutex<std::collections::HashMap<String, crate::hcn::SharedNetwork>>,
    > = std::sync::OnceLock::new();
    let mut networks = NETWORKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    Arc::clone(networks.entry(sid.to_string()).or_default())
}

/// The system services the host gives the user's Android: the backup of its apps' data in
/// `<profile>\DSA Backup`, passkeys (checked here, made and used by Windows Hello in the user's
/// desktop session, which watches for them) and the root key of its KeyMint on its own port.
/// The backup key and the KeyMint root key are sealed in the user's DSA folder
/// (`%LOCALAPPDATA%\DSA\keys`), never on the guest's disk; the KeyMint root key by the
/// computer's TPM first. Backup is off until the user turns it on, which the service keeps in
/// its own settings folder (`settings_dir`); until then neither the backup folder nor its key
/// is made. A part that cannot be set up is logged and left unavailable, which Android reports
/// as such; the rest goes on.
fn serve_host_services(
    session: &Session,
    sid: &str,
    build: &str,
    keymint: VmBoundListener,
    settings_dir: &Path,
) {
    use dsa_host_core::backup::{BackupService, BackupSwitch, FileSwitch, MemorySwitch};
    use dsa_host_core::host_keys::{HostKeyStore, NoKeyStore};
    use dsa_host_core::passkeys::PasskeyService;

    let keys: Arc<dyn HostKeyStore> = match local_app_data(sid) {
        Ok(local) => Arc::new(crate::credential_store::for_user(
            sid,
            &crate::paths::UserRoot::in_local_app_data(&local).keys(),
        )),
        Err(why) => {
            eprintln!(
                "{sid}: keys: the user's DSA folder cannot be found, so no key is kept: {why}"
            );
            Arc::new(NoKeyStore(format!(
                "the folder this user's keys are kept in cannot be found ({why})"
            )))
        }
    };
    session.set_keymint_keys(Arc::clone(&keys));
    session.serve_keymint_port(Box::new(keymint), build);
    match crate::profile::profile_folder(sid) {
        Ok(home) => {
            let folder = dsa_host_core::backup::folder_in(&home);
            let switch: Arc<dyn BackupSwitch> = match FileSwitch::for_user(settings_dir, sid) {
                Ok(switch) => Arc::new(switch),
                Err(why) => {
                    eprintln!("{sid}: backup: the choice cannot be kept, so it is off: {why}");
                    Arc::new(MemorySwitch::default())
                }
            };
            // Made at the first backup, never before the user turns backup on, and used
            // with the user's own rights.
            let owner = sid.to_string();
            let prepare = Arc::new(move |folder: &Path| {
                crate::user_disk::prepare_backup_folder(folder, &owner)
            });
            let access_owner = sid.to_string();
            let access: dsa_host_core::backup::FileAccess =
                Arc::new(move |work: &mut dyn FnMut()| {
                    crate::safe_fs::as_user(&access_owner, work)
                });
            let made_on = crate::computer_identity::computer_name();
            session.serve_backup(Arc::new(
                BackupService::new(folder, keys, &made_on)
                    .with_switch(switch)
                    .with_prepare(prepare)
                    .with_file_access(access),
            ));
        }
        Err(why) => eprintln!("{sid}: backup: the user's backups cannot be kept: {why}"),
    }
    // The root certificates added to the computer's and the user's trust stores.
    session.serve_host_certificates(Arc::new(
        crate::host_certificates::WindowsRootStore::for_user(sid),
    ));
    session.serve_passkeys(Arc::new(PasskeyService::new(Arc::new(
        crate::https::WinHttpsGet::default(),
    ))));
}

/// Holds the folders of a user's Android disk `disk` in their own folder `root`, and checks
/// the disk and the files beside it are plain files, for as long as the service works on them
/// (see `crate::safe_fs`). Without `root`, the disk is in a folder only the service writes.
pub(crate) fn guard_android_disk(
    root: Option<&Path>,
    disk: &Path,
) -> Result<Vec<crate::safe_fs::GuardedDir>, String> {
    let Some(root) = root else {
        return Ok(Vec::new());
    };
    let version = disk.with_extension("image-version");
    let partial = disk.with_extension("vhdx.new");
    crate::safe_fs::guard_files(root, &[disk, &version, &partial])
}

/// The user's Android: their data disk, VM and session, created on their first request.
/// With `power`, the session hears the computer's power from it.
/// With `direct_mapped` (the host's direct-mapped mode is on), each boot maps the read-only
/// disks where it can.
pub fn create_user_android(
    config: &HostConfig,
    build: &str,
    sid: &str,
    updates: Option<&Updates>,
    power: Option<&PowerHub>,
    direct_mapped: Option<&Arc<DirectMappedImages>>,
) -> Result<Arc<dyn UserAndroid>, String> {
    let user = config.user_vm(sid).map_err(|e| e.to_string())?;
    // The folder the user's Android disk is in, when it is the user's own: every file the
    // service touches there is checked and its folders held (see `crate::safe_fs`).
    let user_root = user
        .disk_in_profile
        .then(|| {
            user.config
                .data_disk
                .parent()
                .map(std::path::Path::to_path_buf)
        })
        .flatten();
    prepare_user_disk_folder(&user.config.data_disk, !user.disk_in_profile)?;
    if let Some(root) = &user_root {
        {
            let _guard = crate::safe_fs::guard_below(root, root)?;
            for note in crate::paths::migrate_user_disk(
                &config.machine_root(),
                &crate::paths::UserRoot::from_dir(root),
                sid,
            ) {
                eprintln!("{build}: {note}");
            }
        }
        crate::machine_user_folder::prepare(&config.machine_root(), sid)?;
        move_disks_of(&config.machine_root(), sid, build);
    }
    // Earlier releases wrote each boot's initrd to the service's temporary folder; one a
    // stopped start left there goes.
    let _ = std::fs::remove_file(crate::paths::old_boot_initrd(&user.vm_name));
    // With updates the disk is made when the VM first starts, from the image current then:
    // before the first image is downloaded there is nothing to copy it from yet.
    if updates.is_none() {
        let _guard = guard_android_disk(user_root.as_deref(), &user.config.data_disk)?;
        prepare_user_data_disk(&user.config.data_disk, user.data_disk_size_gb)?;
    }
    let disk_size_gb = user.data_disk_size_gb;
    let console_log = user.console_log.clone();
    let owner = sid.to_string();
    // What this PC says it is, with what the user chose, read at each start.
    let boot_identity = BootIdentity::new(
        Arc::new(FileDeviceIdentities::new(config.settings_dir())),
        sid,
        Arc::new(crate::computer_identity::read),
    );
    // Set each time the guest's configuration is built, from the command line it boots with.
    let trace_on = Arc::new(AtomicBool::new(false));
    let traced = Arc::clone(&trace_on);
    // The VM's memory is sized at each start, so a changed limit, or memory added to the
    // computer, takes effect the next time Android starts.
    let memory_max = crate::android_vm::MemoryMax::default();
    let memory_inputs = || MemoryInputs {
        fixed_mb: config.memory_mb,
        settings: settings_store(config),
        user: sid.to_string(),
        // Memory not in use goes back to Windows, so a VM with no limit may grow to the PC's
        // memory less what Windows keeps.
        ceiling: GrowthCeiling::HostLessReserve,
        facts: Arc::new(crate::power::ComputerFacts),
        hypervisor_max_mb: None,
    };
    let launcher = match updates {
        None => {
            let base = user.config;
            let (memory, max) = (memory_inputs(), memory_max.clone());
            AndroidVmLauncher::with_source(
                &user.vm_name,
                Box::new(move || {
                    let mut vm = base.clone();
                    follow_monitors(&mut vm, &owner);
                    let plan = memory.plan();
                    *max.lock().unwrap_or_else(|e| e.into_inner()) = plan.max_mb;
                    vm.memory_mb = plan.start_mb;
                    traced.store(
                        guest_traces_input(&vm.full_kernel_cmdline()),
                        Ordering::Relaxed,
                    );
                    Ok(vm)
                }),
            )
        }
        Some(updates) => {
            let base = user.config;
            let config = config.clone();
            let root = user_root.clone();
            let max = memory_max.clone();
            let planner = BootPlanner {
                store: Some(Arc::clone(&updates.store)),
                configured_dir: None,
                rules: config.image_rules(),
                check: None,
                data_disk: base.data_disk.clone(),
                data_disk_size_gb: disk_size_gb,
                image_cache: None,
                backend: crate::user_disk::VhdxBackend,
                memory: memory_inputs(),
            };
            AndroidVmLauncher::with_source(
                &user.vm_name,
                Box::new(move || {
                    let mut vm = base.clone();
                    let _guard = guard_android_disk(root.as_deref(), &vm.data_disk)?;
                    let plan = planner.plan()?;
                    let mut files = plan.files.clone();
                    config.fill_initrd(&mut files);
                    *max.lock().unwrap_or_else(|e| e.into_inner()) = plan.memory.max_mb;
                    vm.kernel = files.kernel;
                    vm.initrd = files.initrd;
                    vm.kernel_cmdline = files.kernel_cmdline;
                    vm.system_image = files.system_disk;
                    vm.image_version = Some(plan.cache_key().to_string());
                    follow_monitors(&mut vm, &owner);
                    vm.memory_mb = plan.memory.start_mb;
                    traced.store(
                        guest_traces_input(&vm.full_kernel_cmdline()),
                        Ordering::Relaxed,
                    );
                    Ok(vm)
                }),
            )
        }
    }
    .with_console_log(console_log)
    .with_user_root(user_root.clone())
    .with_networking({
        // NAT unless the user turned advanced networking on; read at each start.
        let store = settings_store(config);
        let user = sid.to_string();
        crate::hcn::VmNetworking::new(
            Box::new(move || {
                store
                    .get(&user)
                    .map(|settings| settings.advanced_networking)
                    .unwrap_or(false)
            }),
            user_network(sid),
        )
    })
    .with_memory_max(memory_max)
    .with_direct_mapped(direct_mapped.cloned())
    .with_boot_dir(config.machine_root().tmp())
    .with_bootconfig({
        // An ARM translator (the package the user installed, else the image's default one) is
        // used only while the user's setting is on, and only from a boot that is told so; so
        // are the Google apps the user installed. Their images are on the add-on disk this
        // boot attaches.
        let store = settings_store(config);
        let user = sid.to_string();
        Box::new(move |vm: &AndroidVmConfig| {
            use dsa_host_core::addon_disk::{AddonDisk, Format};
            let image = vm.initrd.as_deref().and_then(std::path::Path::parent);
            let attached = vm
                .addon_disk
                .as_ref()
                .map(|path| AddonDisk::new(path, Format::Vhd).attached())
                .unwrap_or_default();
            dsa_host_core::bootconfig::for_boot(
                &*store,
                &user,
                image,
                &attached,
                &crate::power::ComputerFacts,
            )
        })
    })
    .with_boot_properties({
        // The user's device identity on this PC, which Android reads once while it boots. The
        // PC is read again at each start, so a renamed PC is followed.
        let identity = Arc::clone(&boot_identity);
        Box::new(move |vm: &AndroidVmConfig| {
            Some(identity.boot_properties_of_initrd(vm.initrd.as_deref()))
        })
    });
    let launcher = if config.keep_window_manager {
        let mut keeper = WindowManagerKeeper::new(UserWindowManager::new(sid));
        launcher.with_prerequisite(Box::new(move || keeper.check(Instant::now())))
    } else {
        launcher
    };
    // What Android logs while it boots explains a boot that stalls; the console no longer
    // carries it.
    let boot_signs = Arc::new(Mutex::new(BootSigns::default()));
    let launcher = launcher.with_log_signs(Arc::clone(&boot_signs));
    let service = Guid::vsock_service(config.control_port);
    let listener = VmBoundListener::new(
        launcher.vm_id_slot(),
        Box::new(move |vm_id| {
            HvSocketListener::bind(vm_id, service).map(|l| Box::new(l) as Box<dyn Listener>)
        }),
    );
    let log_service = Guid::vsock_service(config.log_port);
    let log_listener = VmBoundListener::new(
        launcher.vm_id_slot(),
        Box::new(move |vm_id| {
            HvSocketListener::bind(vm_id, log_service).map(|l| Box::new(l) as Box<dyn Listener>)
        }),
    );
    // The file server of the user's shared folders: a plain byte stream, which the guest's
    // kernel speaks 9P on.
    let share_service = Guid::vsock_service(dsa_host_core::file_share::SHARE_PORT);
    let share_listener = VmBoundListener::new(
        launcher.vm_id_slot(),
        Box::new(move |vm_id| {
            crate::hvsocket::RawHvSocketListener::bind(vm_id, share_service)
                .map(|l| Box::new(l) as Box<dyn Listener>)
        }),
    );
    // The port the guest's control daemon asks for KeyMint's root key on, each boot.
    let keymint_service = Guid::vsock_service(dsa_host_core::keymint_root::KEYMINT_PORT);
    let keymint_listener = VmBoundListener::new(
        launcher.vm_id_slot(),
        Box::new(move |vm_id| {
            HvSocketListener::bind(vm_id, keymint_service).map(|l| Box::new(l) as Box<dyn Listener>)
        }),
    );
    // The port the guest's control daemon hands its DNS queries to, which Windows' DNS client
    // answers, so the hosts file, the NRPT, a VPN's split DNS and DNS filters apply to
    // Android's apps. It serves the NAT network and advanced networking alike.
    let dns_service = Guid::vsock_service(dsa_host_core::dns::DNS_PORT);
    let dns_listener = VmBoundListener::new(
        launcher.vm_id_slot(),
        Box::new(move |vm_id| {
            HvSocketListener::bind(vm_id, dns_service).map(|l| Box::new(l) as Box<dyn Listener>)
        }),
    );
    let session_config = config.session_config();
    let handshake = ControlHandshake::new(build, session_config.connect_timeout);
    let session = Session::new(launcher, Box::new(listener), handshake, session_config);
    // The PC's name becomes the guest's device name.
    session.set_device_name_source(Arc::new(move || boot_identity.device_name()));
    // Developer mode's bridge reaches the guest's adbd over a Hyper-V socket to its vsock port.
    session.set_adb_connect(Arc::new(|vm_id: &str| {
        let vm: Guid = vm_id.parse().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{vm_id} is not a VM's runtime ID"),
            )
        })?;
        crate::hvsocket::connect_stream(
            vm,
            Guid::vsock_service(dsa_host_core::adb_bridge::GUEST_ADB_PORT),
            Duration::from_secs(5),
        )
    }));
    // Developer mode's port is attached to the user's own adb server, and advertised over
    // mDNS on the loopback address, so adb finds it without `adb connect`.
    session.set_adb_server(crate::adb_discovery::adb_server_of(sid));
    session.set_adb_advertise(crate::adb_discovery::advertiser());
    session.serve_log_port(Box::new(log_listener), build);
    // The user's folders, served with the user's own rights.
    session.serve_file_shares(
        Box::new(share_listener),
        Arc::new(crate::file_share::UserFolders::new(sid)),
    );
    // Android's shared storage, which Explorer opens through a loopback port.
    session.set_android_files_connect(Arc::new(|vm_id: &str| {
        let vm: Guid = vm_id.parse().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{vm_id} is not a VM's runtime ID"),
            )
        })?;
        crate::hvsocket::connect_stream(
            vm,
            Guid::vsock_service(dsa_host_core::file_share::ANDROID_FILES_PORT),
            Duration::from_secs(5),
        )
    }));
    // The clock lines go to the service's log, beside the guest's console log.
    let user_name = sid.to_string();
    session.set_clock_trace(Some(ClockTrace::new(
        trace_on,
        Arc::new(PerformanceCounterClock),
        Arc::new(move |line: &str| eprintln!("{user_name}: {line}")),
    )));
    // An app's su call is decided by the user's settings, and asked of the computer's
    // administrator through UAC under the app's own name and icon; each decision goes to this
    // log. While a screen reader runs in the user's session the prompt has longer, as hearing
    // it read out takes longer than reading it; that is asked as each prompt opens.
    let reader_sid = sid.to_string();
    session.set_root_approval(Arc::new(
        dsa_host_core::root_approval::RootApproval::new(
            sid,
            settings_store(config),
            Arc::new(crate::root_approval::UacPrompt::new(sid)),
            dsa_host_core::root_approval::service_log(),
        )
        .with_screen_reader(Arc::new(move || {
            crate::root_approval::screen_reader_running(&reader_sid)
        })),
    ));
    serve_host_services(
        &session,
        sid,
        build,
        keymint_listener,
        &config.settings_dir(),
    );
    session.serve_dns_port(
        Box::new(dns_listener),
        Arc::new(crate::dns_resolver::WindowsResolver),
        build,
    );
    let session = Arc::new(session);
    {
        let (watched, user_name) = (Arc::downgrade(&session), sid.to_string());
        if let Err(error) = std::thread::Builder::new()
            .name("dsa-boot-signs".into())
            .spawn(move || watch_boot_signs(watched, boot_signs))
        {
            eprintln!(
                "{user_name}: guest log: cannot read Android's boot for its problems: {error}"
            );
        }
    }
    if let Some(power) = power {
        power.attach(&session);
    }
    // The guest's display refreshes as fast as the user's fastest monitor, which their session
    // records; energy saver, which comes with the power, caps it at 60 Hz.
    let (watched, owner) = (Arc::downgrade(&session), sid.to_string());
    if let Err(error) = std::thread::Builder::new()
        .name("dsa-monitor-refresh".into())
        .spawn(move || {
            dsa_host_core::guest_refresh::follow_monitors(
                watched,
                || {
                    local_app_data(&owner)
                        .ok()
                        .and_then(|dir| crate::monitors::recorded_refresh(&dir))
                },
                dsa_host_core::guest_refresh::MONITOR_POLL,
            )
        })
    {
        eprintln!("{sid}: display: cannot follow the monitors' refresh rate: {error}");
    }
    // The guest's power HAL: its hints set how hard Windows runs the guest's processors.
    let hints = session.watch_power_hints();
    let (watched, user_name) = (Arc::downgrade(&session), sid.to_string());
    if let Err(error) = std::thread::Builder::new()
        .name("dsa-guest-power".into())
        .spawn(move || crate::guest_performance::follow(hints, watched, user_name))
    {
        eprintln!("{sid}: power: cannot follow Android's power hints: {error}");
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

/// Starts the maker of direct-mapped system disks when the host's direct-mapped mode is on:
/// it keeps the uncompressed copy of the current image's system disk in the machine's
/// `direct-mapped` folder, made in the background after the image was installed or downloaded
/// and checked.
fn start_direct_mapped(
    config: &HostConfig,
    updates: Option<&Updates>,
    build: &str,
) -> Option<Arc<DirectMappedImages>> {
    if !config.direct_mapped_images {
        return None;
    }
    let configured = config.system_image.clone();
    let log_build = build.to_string();
    Some(crate::pmem_disk::start(
        &config.machine_root().direct_mapped(),
        crate::pmem_disk::current_image(updates.map(|u| Arc::clone(&u.store)), configured),
        Arc::new(move |line: &str| eprintln!("{log_build}: {line}")),
    ))
}

/// Clears what earlier runs left in the machine's `tmp` folder and makes sure only SYSTEM and
/// Administrators can write there. Problems are logged; the VMs then fail to start with the
/// reason, rather than the service.
fn prepare_machine_tmp(config: &HostConfig, build: &str) {
    let tmp = config.machine_root().tmp();
    for why in crate::paths::clear_tmp(&tmp) {
        eprintln!("{build}: {why}");
    }
    if let Err(why) = std::fs::create_dir_all(&tmp) {
        eprintln!("{build}: creating {}: {why}", tmp.display());
        return;
    }
    if let Err(why) = crate::user_disk::protect_folder_as(&tmp, crate::user_disk::MACHINE_TMP_SDDL)
    {
        eprintln!("{build}: {why}");
    }
}

/// Earlier releases turned the traces on with empty files named `input-trace` and
/// `resize-trace` in the machine's DSA folder and in each user's. A file that is there turns
/// its trace setting on, and goes once the setting is saved; if it cannot be saved the files
/// stay, to be taken over at the next start.
fn take_old_trace_markers(config: &HostConfig, store: &dyn SettingsStore, build: &str) {
    use crate::paths::{default_local_app_data, old_trace_markers, UserRoot};
    let mut markers = config.machine_root().old_trace_markers().to_vec();
    for (sid, profile) in crate::profile::profiles() {
        let local = local_app_data(&sid).unwrap_or_else(|_| default_local_app_data(&profile));
        markers.extend(UserRoot::in_local_app_data(&local).old_trace_markers());
    }
    let old = old_trace_markers(&markers);
    if old.found.is_empty() {
        return;
    }
    let mut machine = store.machine();
    machine.input_trace |= old.input_trace;
    machine.resize_trace |= old.resize_trace;
    if let Err(why) = store.set_machine(machine) {
        eprintln!("{build}: the trace switch files stay, the settings could not be saved: {why}");
        return;
    }
    for marker in &old.found {
        // A marker in a user's folder is removed as itself, never through a link.
        match crate::safe_fs::remove_plain_file(marker) {
            Ok(_) => eprintln!(
                "{build}: {} is now the trace setting, and the file is removed",
                marker.display()
            ),
            Err(why) => eprintln!("{build}: removing {}: {why}", marker.display()),
        }
    }
}

/// Moves the add-on and pstore disks of every user whose Android disk is in their profile out
/// of their folder into the machine's (see [`crate::machine_user_folder`]), then checks the
/// images on each add-on disk at low priority, so their first start need not.
fn move_user_disks(config: &HostConfig, build: &str) {
    let users: Vec<String> = crate::profile::profiles()
        .into_iter()
        .map(|(sid, _)| sid)
        .filter(|sid| crate::vm::is_sid(sid))
        .filter(|sid| config.user_vm(sid).is_ok_and(|user| user.disk_in_profile))
        .collect();
    let machine = config.machine_root();
    for sid in &users {
        move_disks_of(&machine, sid, build);
    }
    let config = config.clone();
    let worker_build = build.to_string();
    let spawned = thread::Builder::new()
        .name("dsa-addon-check".into())
        .spawn(move || {
            use windows_sys::Win32::System::Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
            };
            // SAFETY: lowers this thread's own priority, for CPU and for disk reads.
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
            for sid in users {
                if let Some(path) = config.user_vm(&sid).ok().and_then(|u| u.config.addon_disk) {
                    let disk = dsa_host_core::addon_disk::AddonDisk::new(
                        path,
                        dsa_host_core::addon_disk::Format::Vhd,
                    );
                    if disk.path().exists() {
                        let intact = disk.verify();
                        eprintln!(
                            "{worker_build}: {sid}: the add-on disk is checked; {} intact images",
                            intact.len()
                        );
                    }
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("{build}: the add-on disks are checked at each start instead: {error}");
    }
}

/// Moves one user's add-on and pstore disks out of their folder, as [`move_user_disks`] does.
fn move_disks_of(machine: &crate::paths::MachineRoot, sid: &str, build: &str) {
    let Ok(local) = local_app_data(sid) else {
        return;
    };
    let root = crate::paths::UserRoot::in_local_app_data(&local);
    for note in crate::machine_user_folder::migrate_disks(machine, &root, sid) {
        eprintln!("{build}: {sid}: {note}");
    }
}

/// The folder of the image a user's Android starts from next, which holds its `image.json`.
fn current_image_dir(
    config: &HostConfig,
    images: Option<&ImageStore>,
) -> Option<std::path::PathBuf> {
    // Without an update store the image is the one the configuration names.
    let configured = config
        .image_files(None)
        .ok()
        .and_then(|files| files.initrd)
        .and_then(|initrd| initrd.parent().map(Path::to_path_buf));
    Some(current_image(images, configured.as_deref()).ok()?.dir)
}

/// The build of the image a user's Android starts from next, from its `image.json`.
fn current_image_build(config: &HostConfig, images: Option<&ImageStore>) -> Option<ImageBuild> {
    ImageBuild::from_image_dir(&current_image_dir(config, images)?)
}

/// The render GPU a user chose in their own `compositor.json` before they first saved
/// settings, found by their SID.
fn previous_gpu_of_sid() -> dsa_host_core::settings::PreviousRenderGpu {
    Box::new(|sid: &str| {
        crate::profile::local_app_data(sid)
            .ok()
            .and_then(|dir| previous_render_gpu(&dir))
    })
}

/// The settings in the service's own folder. A user who has never saved settings starts
/// from the render GPU in their own `compositor.json`, if they chose one there. A computer
/// whose settings record no release channel yet is on the channel of the installer that put
/// DSA here.
fn settings_store(config: &HostConfig) -> Arc<dyn SettingsStore> {
    dsa_host_core::host_config::settings_store(
        config.settings_dir(),
        Some(config.installer_channel(crate::VERSION)),
        Some(previous_gpu_of_sid()),
    )
}

/// [`settings_store`], with the release channel recorded first when the settings record none
/// yet: the installer's, so that every later upgrade keeps it.
fn service_settings_store(config: &HostConfig, build: &str) -> Arc<dyn SettingsStore> {
    dsa_host_core::host_config::service_settings_store(
        config.settings_dir(),
        config.installer_channel(crate::VERSION),
        Some(previous_gpu_of_sid()),
        build,
    )
}

/// Takes the pages of the disks each running VM maps directly (the host's direct-mapped mode)
/// out of the VM's working set until `stop` is set, as [`MappedPagesTrim`] decides: the pages
/// Android read once, at boot or when an app started, would otherwise stay in `vmmem` until
/// Windows ran short of memory (see [`crate::mapped_pages`]). Only the views of the
/// machine's direct-mapped disks and of the user's add-on disk are touched.
///
/// [`MappedPagesTrim`]: dsa_host_core::direct_mapped::MappedPagesTrim
fn give_back_mapped_pages(
    androids: &dyn UserAndroids,
    root: &crate::paths::MachineRoot,
    build: &str,
    stop: &AtomicBool,
) {
    use dsa_host_core::direct_mapped::{MappedPagesTrim, MAPPED_PAGES_CHECK};
    let mut trims: std::collections::HashMap<(String, String), MappedPagesTrim> =
        std::collections::HashMap::new();
    let mut failed: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut next = Instant::now() + MAPPED_PAGES_CHECK;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        if Instant::now() < next {
            continue;
        }
        next = Instant::now() + MAPPED_PAGES_CHECK;
        let mut running = std::collections::HashSet::new();
        for (user, state) in androids.running() {
            if state == dsa_host_core::session::SessionState::Stopped {
                continue;
            }
            let Some(vm_id) = androids.android(&user).ok().and_then(|a| a.vm_id()) else {
                continue;
            };
            let key = (user.clone(), vm_id.clone());
            running.insert(key.clone());
            let pids = crate::host_memory::vm_processes(&vm_id).unwrap_or_default();
            for pid in pids {
                let Some((_, before)) = crate::host_memory::shared_working_set(pid) else {
                    continue;
                };
                let now = Instant::now();
                let trim = trims.entry(key.clone()).or_default();
                if !trim.due(now, before) {
                    continue;
                }
                let disks = root.direct_mapped();
                let addons = root.user_addon_disk(&user);
                match crate::mapped_pages::give_back(pid, &[&disks, &addons]) {
                    Ok(_) => {
                        let after = crate::host_memory::shared_working_set(pid)
                            .map_or(before, |(_, shared)| shared);
                        if let Some(line) = trim.gave_back(now, before, after) {
                            eprintln!("{build}: {user}: {line}");
                        }
                    }
                    Err(why) => {
                        // Said once a boot; tried again after the gap.
                        trim.failed(now);
                        if failed.insert(key.clone()) {
                            eprintln!(
                                "{build}: {user}: direct-mapped: the mapped disks' pages cannot \
                                 be taken out of the VM's working set ({why}); they stay until \
                                 Windows is short of memory"
                            );
                        }
                    }
                }
            }
        }
        trims.retain(|key, _| running.contains(key));
        failed.retain(|key| running.contains(key));
    }
}

/// Serves the local API until `stop` is set, then stops every user's VM.
pub fn serve(config: HostConfig, build: &str, stop: Arc<AtomicBool>) -> Result<(), String> {
    // A service, yet every pointer event passes through this process.
    if let Err(why) = crate::scheduling::raise_this_process() {
        eprintln!("{why}");
    }
    serve_with(config, build, stop, register_graphics_service_guids)
}

/// Serves the local API with custom graphics service GUID registration.
pub fn serve_with<F>(
    config: HostConfig,
    build: &str,
    stop: Arc<AtomicBool>,
    register_graphics_services: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    if let Err(error) = register_graphics_services() {
        eprintln!("{build}: registering graphics service GUIDs: {error}");
    }
    let sddl = config.pipe_sddl.as_deref().unwrap_or(DEFAULT_PIPE_SDDL);
    let listener = NamedPipeListener::bind(&config.pipe_name, sddl)
        .map_err(|e| format!("opening the pipe {:?}: {e}", config.pipe_name))?;
    eprintln!(
        "{build}: serving the local API on \\\\.\\pipe\\{}",
        config.pipe_name
    );
    if config.test_package {
        eprintln!(
            "{build}: this is a TEST PACKAGE made to check the installer: its kernel and \
             graphics library are empty stand-ins and Android cannot run from it. Install \
             the real package instead"
        );
    }
    for note in &config.package_notes {
        eprintln!("{build}: {note}");
    }
    prepare_machine_tmp(&config, build);
    let grace = Duration::from_secs(config.stop_timeout_secs + 15);
    let store = service_settings_store(&config, build);
    // A store that cannot be opened turns image updates off; Android still starts from the
    // image it has, and the reason is shown with the Android version.
    let (updates, updates_disabled) = match open_updates_or_disable(updates_config(&config), build)
    {
        Ok(updates) => (updates, None),
        Err(disabled) => (None, Some(disabled)),
    };
    let image_updates_available = updates.is_some() && config.image_updates_available();
    let direct_mapped = start_direct_mapped(&config, updates.as_ref(), build);
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
            .spawn(move || run_update_checks(check, &WinHttpFetch::default(), stop))
        {
            eprintln!("{build}: starting the update check failed: {error}");
        }
    }
    let program = match config.program_settings(crate::VERSION) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{build}: program updates disabled: {error}");
            None
        }
    };
    let facts = ServiceFacts {
        version: crate::VERSION.into(),
        log_dir: config
            .log_dir()
            .map(|dir| dir.to_string_lossy().into_owned()),
        program_updates_available: program.is_some() && config.program_updates_available(),
        update_channel_available: program.is_some() && config.program_updates_available(),
        image_updates_available,
        android_version: config
            .bundled_image
            .as_ref()
            .or(config.first_image.as_ref())
            .map(|image| image.version.clone()),
    };
    // With an update store the image the next start boots is the store's current one, which
    // an update changes while the service runs.
    let android_version: Option<dsa_host_core::host_api::AndroidVersionSource> =
        updates.as_ref().map(|updates| {
            let store = Arc::clone(&updates.store);
            Arc::new(move || store.state().ok().map(|state| state.current.version))
                as dsa_host_core::host_api::AndroidVersionSource
        });
    let program_status = Arc::new(ProgramStatus::default());
    // Every user's Android follows the computer's power.
    let power = Arc::new(PowerHub::new());
    {
        let (power, stop) = (Arc::clone(&power), Arc::clone(&stop));
        if let Err(error) = thread::Builder::new()
            .name("dsa-power".into())
            .spawn(move || crate::power::follow(power, stop))
        {
            eprintln!("{build}: following the computer's power failed: {error}");
        }
    }
    // And its sleep: kept for as long as the service runs.
    let _sleep_watch = match crate::power::watch_sleep(Arc::clone(&power)) {
        Ok(watch) => Some(watch),
        Err(why) => {
            eprintln!("{build}: Android will not hear when the computer sleeps: {why}");
            None
        }
    };
    let identity_images = updates.as_ref().map(|updates| Arc::clone(&updates.store));
    let translator_images = updates.as_ref().map(|updates| Arc::clone(&updates.store));
    // A data reset makes the fresh disk from the image the next start boots.
    let reset_images = updates.as_ref().map(|updates| Arc::clone(&updates.store));
    let factory_config = config.clone();
    let factory_build = build.to_string();
    let factory_status = program.as_ref().map(|_| Arc::clone(&program_status));
    let factory_store = Arc::clone(&store);
    let version_store = updates.as_ref().map(|updates| StoreVersions {
        store: Arc::clone(&updates.store),
        notices: Arc::clone(&updates.notices),
        check_now: Arc::clone(&updates.check_now),
        shared_by_users: true,
    });
    let androids = Arc::new(PerUser::new(move |sid: &str| {
        let android = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            create_user_android(
                &factory_config,
                &factory_build,
                sid,
                updates.as_ref(),
                Some(power.as_ref()),
                direct_mapped.as_ref(),
            )
        })) {
            Ok(res) => res,
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                Err(format!("setting up Android for {sid} panicked: {msg}"))
            }
        };
        match &android {
            // Freezing is the user's choice, kept with their settings; off unless they chose it.
            // So is clipboard sharing, kept with them too; on unless they turned it off.
            // Opening web links in Android rather than the browser is theirs too; off unless they
            // chose it.
            Ok(android) => {
                let settings = factory_store.get(sid).ok();
                android.freeze_when_idle(settings.as_ref().is_some_and(|s| s.freeze));
                android.share_clipboard(settings.as_ref().is_none_or(|s| s.clipboard));
                android.links_in_android(settings.as_ref().is_some_and(|s| s.links_in_android));
                // So is restarting into a downloaded Android image by itself; on unless they
                // turned it off.
                android.image_auto_restart(settings.as_ref().is_none_or(|s| s.image_auto_restart));
                // So is trusting the root certificates added to the computer; on unless they
                // turned it off.
                android
                    .trust_host_certificates(settings.as_ref().is_none_or(|s| s.host_certificates));
                // So is file sharing, on with every folder unless they chose otherwise.
                android.share_folders(
                    settings.as_ref().is_none_or(|s| s.file_sharing),
                    settings
                        .as_ref()
                        .map_or_else(dsa_host_core::file_share::all_folders, |s| {
                            s.shared_folders.clone()
                        }),
                );
                // So is attaching developer mode's port to their adb server, on unless they
                // turned it off.
                android.adb_auto_connect(settings.as_ref().is_none_or(|s| s.adb_auto_connect));
                // So is developer mode, off unless they turned it on, and its adb port.
                if let Some(settings) = settings.as_ref().filter(|s| s.developer_mode) {
                    if let Err(why) = android.developer_mode(true, settings.adb_port) {
                        eprintln!("{factory_build}: developer mode for {sid}: {why}");
                    }
                }
                // Root elevation: the computer's switch, and the level of each of their apps.
                dsa_host_core::root_elevation::apply(android.as_ref(), factory_store.as_ref(), sid);
            }
            Err(why) => eprintln!("{factory_build}: no Android for {sid}: {why}"),
        }
        // Tells about updates of DSA itself, and holds starts back while setup runs.
        match &factory_status {
            Some(status) => android.map(|android| {
                Arc::new(WithProgramStatus {
                    android,
                    status: Arc::clone(status),
                }) as Arc<dyn UserAndroid>
            }),
            None => android,
        }
    }));
    // Android's files at the address Explorer's navigation pane keeps: open for as long as the
    // service runs, whether or not anyone's Android is.
    let _files_door = match dsa_host_core::files_door::FilesDoor::start(
        dsa_host_core::files_door::DOOR_PORT,
        Arc::new(crate::android_files::owner_of_key),
        androids.clone(),
        dsa_host_core::files_door::START_WAIT,
    ) {
        Ok(door) => Some(door),
        Err(why) => {
            eprintln!(
                "{build}: Android's files cannot be shown in Explorer's navigation pane: \
                 loopback port {} is taken: {why}",
                dsa_host_core::files_door::DOOR_PORT
            );
            None
        }
    };
    take_old_trace_markers(&config, &*store, build);
    move_user_disks(&config, build);
    let host_memory: Arc<dyn HostMemory> = Arc::new(WindowsHostMemory);
    let mut service = HostApiService::new(androids.clone(), build)
        .with_data_reset(Arc::new(crate::data_reset::windows_data_reset(
            config.clone(),
            reset_images,
        )))
        .with_settings(Arc::clone(&store), facts)
        .with_host_memory(Arc::clone(&host_memory))
        // The network of each user's running Android: its mode, addresses and DNS servers.
        .with_network(Arc::new(
            |sid: &str, _: &dsa_host_core::settings::Settings| {
                crate::hcn::shared_status(&user_network(sid))
            },
        ))
        .with_widget_placements(Arc::new(dsa_host_core::widgets::FileWidgetPlacements::new(
            config.settings_dir(),
        )))
        .with_host_logs({
            let config = config.clone();
            Arc::new(move |sid: &str| host_logs_of(&config, sid))
        })
        .with_device_identities(
            Arc::new(FileDeviceIdentities::new(config.settings_dir())),
            {
                let config = config.clone();
                Arc::new(move |_: &str| current_image_build(&config, identity_images.as_deref()))
            },
        )
        // The defaults the settings show are what this PC says it is now.
        .with_computer_identity(Arc::new(crate::computer_identity::read))
        // The default ARM translator of the image the next start boots: only an x86_64 image
        // has one.
        .with_default_arm_translation({
            let config = config.clone();
            Arc::new(move |_: &str| {
                current_image_dir(&config, translator_images.as_deref())
                    .and_then(|dir| dsa_host_core::arm_translation::default_package(&dir))
            })
        });
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
    if config.direct_mapped_images {
        let androids = androids.clone();
        let (worker_build, stop, root) =
            (build.to_string(), Arc::clone(&stop), config.machine_root());
        if let Err(error) = thread::Builder::new()
            .name("dsa-mapped-pages".into())
            .spawn(move || give_back_mapped_pages(&*androids, &root, &worker_build, &stop))
        {
            eprintln!("{build}: starting the give-back of the mapped disks' pages failed: {error}");
        }
    }
    if let Some(source) = android_version {
        service = service.with_android_version(source);
    }
    // Google Play, for a user who asks: downloaded from LiteGapps and made ready in the
    // service's own temporary folder, and the installed package kept in the machine's DSA
    // folder, which outlasts the service's restarts and updates.
    service = service.with_gapps(
        dsa_host_core::gapps::Source::new(
            Arc::new(UrlFetch::default()),
            config.machine_root().tmp().join("gapps"),
        )
        .with_package_dir(config.machine_root().gapps()),
    );
    // Each user's add-ons, on their add-on disk, which their VM attaches read only: in the
    // machine's folder for them when their Android disk is in their profile, else beside it.
    service = service.with_addon_disks({
        let config = config.clone();
        Arc::new(move |sid: &str| {
            let vm = config.user_vm(sid).map_err(|e| e.to_string())?;
            if vm.disk_in_profile {
                crate::machine_user_folder::prepare(&config.machine_root(), sid)?;
            }
            let path = vm
                .config
                .addon_disk
                .ok_or_else(|| format!("{sid}'s VM has no add-on disk"))?;
            Ok(dsa_host_core::addon_disk::AddonDisk::new(
                path,
                dsa_host_core::addon_disk::Format::Vhd,
            ))
        })
    });
    if let Some(versions) = version_store {
        service = service.with_android_versions(Arc::new(versions));
    } else if let Some(disabled) = updates_disabled {
        service = service.with_android_versions(Arc::new(disabled));
    }
    if let Some(settings) = program {
        let check_interval = config
            .update
            .as_ref()
            .map(|update| update.check_interval_hours.max(1))
            .unwrap_or(24);
        // Setup replaces this program's file; the service reads its version from there and
        // restarts into it when it is newer than this process.
        let installer = match std::env::current_exe() {
            Ok(program) => SetupInstaller::for_service(program, Arc::clone(&stop)),
            Err(error) => {
                eprintln!("{build}: this service cannot find its own program file: {error}");
                SetupInstaller::default()
            }
        };
        let updates = ProgramUpdates::new(
            settings,
            Box::new(UrlFetch::default()),
            Box::new(installer),
            androids.clone(),
            program_status,
            build,
        );
        let switch = Arc::clone(&store);
        updates.follow_switch(Box::new(move || switch.machine().program_updates));
        let channel = Arc::clone(&store);
        updates.follow_channel(Box::new(move || channel.machine().release_channel()));
        service = service.with_program_updates(updates.clone());
        let stop = Arc::clone(&stop);
        if let Err(error) = thread::Builder::new()
            .name("dsa-program-updates".into())
            .spawn(move || {
                updates.run(
                    &stop,
                    Timing {
                        first_check: Duration::from_secs(60),
                        check_interval: Duration::from_secs(check_interval * 3600),
                        retry: Duration::from_secs(60),
                    },
                )
            })
        {
            eprintln!("{build}: starting the program update check failed: {error}");
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
