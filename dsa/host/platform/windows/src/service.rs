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
//! The settings users change from the settings app live in the service's own folder
//! (`settings_dir`). Turning either kind of update off there stops the checks while the
//! service runs; the render GPU is for each user's window manager, which asks for it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{HostApiService, PerUser, ServiceFacts, UserAndroid, UserAndroids};
use dsa_host_core::memory::{summary_lines, HostMemory, LOG_INTERVAL};
use dsa_host_core::session::Session;
use dsa_host_core::settings::{FileSettings, SettingsStore};
use dsa_host_core::update::program_service::{
    ProgramStatus, ProgramUpdates, Timing, WithProgramStatus,
};
use dsa_host_core::update::service::{
    check_reporting, next_image_check, Download, UpdateNotices, UpdatingAndroid,
};
use dsa_host_core::update::store::ImageStore;
use dsa_transport::Listener;

use crate::android_vm::AndroidVmLauncher;
use crate::guest_communication_services::register_graphics_service_guids;
use crate::guid::Guid;
use crate::host_config::HostConfig;
use crate::host_memory::WindowsHostMemory;
use crate::https::{UrlFetch, WinHttpFetch};
use crate::hvsocket::HvSocketListener;
use crate::pipe::{NamedPipeListener, DEFAULT_PIPE_SDDL};
use crate::render_gpu::previous_render_gpu;
use crate::setup::SetupInstaller;
use crate::user_disk::{bring_disk_to, prepare_user_data_disk, prepare_user_disk_folder};
use crate::vm_listener::VmBoundListener;

/// What the service shares between users when it updates the image.
#[derive(Clone)]
pub struct Updates {
    pub store: Arc<ImageStore>,
    pub notices: Arc<UpdateNotices>,
    pub boot_timeout: Duration,
    pub trial_running: Arc<AtomicBool>,
}

/// The user's Android: their data disk, VM and session, created on their first request.
pub fn create_user_android(
    config: &HostConfig,
    build: &str,
    sid: &str,
    updates: Option<&Updates>,
) -> Result<Arc<dyn UserAndroid>, String> {
    let user = config.user_vm(sid).map_err(|e| e.to_string())?;
    prepare_user_disk_folder(&user.config.data_disk, !user.disk_in_profile)?;
    // With updates the disk is made when the VM first starts, from the image current then:
    // before the first image is downloaded there is nothing to copy it from yet.
    if updates.is_none() {
        prepare_user_data_disk(&user.config.data_disk, user.data_disk_size_gb)?;
    }
    let disk_size_gb = user.data_disk_size_gb;
    let console_log = user.console_log.clone();
    let launcher = match updates {
        None => AndroidVmLauncher::new(&user.vm_name, user.config),
        Some(updates) => {
            let store = Arc::clone(&updates.store);
            let base = user.config;
            let config = config.clone();
            AndroidVmLauncher::with_source(
                &user.vm_name,
                Box::new(move || {
                    let current = store.state().map_err(|e| e.to_string())?.current;
                    let files = config.image_files(Some(&store.image_dir(&current.version)));
                    let mut vm = base.clone();
                    match &files.disk_template {
                        // A new user's disk is the template; an existing one gets this
                        // image's Android and keeps the user's data.
                        Some(template) => bring_disk_to(&vm.data_disk, template, &current.version)?,
                        None => prepare_user_data_disk(&vm.data_disk, disk_size_gb)?,
                    }
                    vm.kernel = files.kernel;
                    vm.initrd = files.initrd;
                    vm.kernel_cmdline = files.kernel_cmdline;
                    vm.system_image = files.system_image;
                    vm.image_version = Some(current.version.clone());
                    Ok(vm)
                }),
            )
        }
    }
    .with_console_log(console_log);
    let service = Guid::vsock_service(config.control_port);
    let listener = VmBoundListener::new(
        launcher.vm_id_slot(),
        Box::new(move |vm_id| {
            HvSocketListener::bind(vm_id, service).map(|l| Box::new(l) as Box<dyn Listener>)
        }),
    );
    let session_config = config.session_config();
    let handshake = ControlHandshake::new(build, session_config.connect_timeout);
    let session = Session::new(launcher, Box::new(listener), handshake, session_config);
    Ok(match updates {
        None => Arc::new(session),
        Some(updates) => Arc::new(UpdatingAndroid {
            session: Arc::new(session),
            store: Arc::clone(&updates.store),
            notices: Arc::clone(&updates.notices),
            boot_timeout: updates.boot_timeout,
            trial_running: Arc::clone(&updates.trial_running),
        }),
    })
}

/// Opens the update store when the configuration names one.
fn open_updates(config: &HostConfig, build: &str) -> Result<Option<Updates>, String> {
    let (Some(root), Some(first)) = (&config.images_dir, &config.first_image) else {
        return Ok(None);
    };
    let store = ImageStore::open_as(root, first, build).map_err(|e| e.to_string())?;
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
    }))
}

/// Checks the release list a minute after the service starts and then every
/// `check_interval_hours`, until `stop` is set. Without an image to boot (an installation
/// that bundled none), it checks at once and again every few minutes until one is in place,
/// and tells the user what it is downloading.
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
    while !stop.load(Ordering::Relaxed) {
        next = next_image_check(next, Instant::now(), !no_image(), FIRST_IMAGE_RETRY);
        if Instant::now() < next {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        let waiting_for_first = no_image();
        // Turned off in the settings, the list is not read, except for the first image,
        // without which no Android can start. A minute later it looks again, so turning
        // updates back on takes effect soon.
        if !waiting_for_first && !store.machine().image_updates {
            next = Instant::now() + SETTINGS_RECHECK;
            continue;
        }
        // The notice changes with every whole percent; the log gets the start only.
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
        match check_reporting(
            &updates.store,
            &WinHttpFetch::default(),
            &settings.list_url,
            &keys,
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
                        "the Android image could not be downloaded yet ({error}); \
                         the service tries again in {} minutes",
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

/// How soon the service tries again while it has no image to boot.
const FIRST_IMAGE_RETRY: Duration = Duration::from_secs(300);

/// How soon an update check that the settings turned off looks at them again.
const SETTINGS_RECHECK: Duration = Duration::from_secs(60);

/// Where the settings are kept when the configuration names no folder.
fn default_settings_dir() -> std::path::PathBuf {
    std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("C:\\ProgramData"))
        .join("DSA")
        .join("settings")
}

/// The settings in the service's own folder. A user who has never saved settings starts
/// from the render GPU in their own `compositor.json`, if they chose one there.
fn settings_store(config: &HostConfig) -> Arc<dyn SettingsStore> {
    let dir = config
        .settings_dir
        .clone()
        .unwrap_or_else(default_settings_dir);
    Arc::new(
        FileSettings::new(dir).with_previous_render_gpu(Box::new(|sid: &str| {
            crate::profile::local_app_data(sid)
                .ok()
                .and_then(|dir| previous_render_gpu(&dir))
        })),
    )
}

/// Logs the memory of every running user's Android every few minutes until `stop` is set:
/// on the host, its VM and window processes, and in the guest, what it last reported.
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

/// Serves the local API until `stop` is set, then stops every user's VM.
pub fn serve(config: HostConfig, build: &str, stop: Arc<AtomicBool>) -> Result<(), String> {
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
    let grace = Duration::from_secs(config.stop_timeout_secs + 15);
    let store = settings_store(&config);
    let updates = match open_updates(&config, build) {
        Ok(updates) => updates,
        Err(error) => {
            eprintln!("{build}: image updates disabled: {error}");
            None
        }
    };
    let image_updates_available = updates.is_some() && config.image_updates_available();
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
        image_updates_available,
    };
    let program_status = Arc::new(ProgramStatus::default());
    let factory_config = config.clone();
    let factory_build = build.to_string();
    let factory_status = program.as_ref().map(|_| Arc::clone(&program_status));
    let androids = Arc::new(PerUser::new(move |sid: &str| {
        let android = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            create_user_android(&factory_config, &factory_build, sid, updates.as_ref())
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
        if let Err(why) = &android {
            eprintln!("{factory_build}: no Android for {sid}: {why}");
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
    // %ProgramData%\DSA\resize-trace turns on the per-stage resize trace lines.
    let resize_trace_file = std::env::var_os("ProgramData")
        .map(|data| {
            std::path::PathBuf::from(data)
                .join("DSA")
                .join(dsa_host_core::resize_trace::DETAIL_FILE)
                .exists()
        })
        .unwrap_or(false);
    let host_memory: Arc<dyn HostMemory> = Arc::new(WindowsHostMemory);
    let mut service = HostApiService::new(androids.clone(), build)
        .with_resize_trace_detail(resize_trace_file)
        .with_settings(Arc::clone(&store), facts)
        .with_host_memory(Arc::clone(&host_memory));
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
    if let Some(settings) = program {
        let check_interval = config
            .update
            .as_ref()
            .map(|update| update.check_interval_hours.max(1))
            .unwrap_or(24);
        let updates = ProgramUpdates::new(
            settings,
            Box::new(UrlFetch::default()),
            Box::new(SetupInstaller),
            androids.clone(),
            program_status,
            build,
        );
        let switch = Arc::clone(&store);
        updates.follow_switch(Box::new(move || switch.machine().program_updates));
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
