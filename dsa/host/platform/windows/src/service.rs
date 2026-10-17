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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{HostApiService, PerUser, UserAndroid};
use dsa_host_core::session::Session;
use dsa_host_core::update::service::{check, UpdateNotices, UpdatingAndroid};
use dsa_host_core::update::store::ImageStore;
use dsa_transport::Listener;

use crate::android_vm::AndroidVmLauncher;
use crate::guid::Guid;
use crate::host_config::HostConfig;
use crate::https::WinHttpFetch;
use crate::hvsocket::HvSocketListener;
use crate::pipe::{NamedPipeListener, DEFAULT_PIPE_SDDL};
use crate::user_disk::prepare_user_data_disk;
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
    prepare_user_data_disk(&user.config.data_disk, user.data_disk_size_gb)?;
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
                    let (kernel, system_image) =
                        config.image_files(Some(&store.image_dir(&current.version)));
                    let mut vm = base.clone();
                    vm.kernel = kernel;
                    vm.system_image = system_image;
                    Ok(vm)
                }),
            )
        }
    };
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
fn open_updates(config: &HostConfig) -> Result<Option<Updates>, String> {
    let (Some(root), Some(first)) = (&config.images_dir, &config.first_image) else {
        return Ok(None);
    };
    let store = ImageStore::open(root, first).map_err(|e| e.to_string())?;
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
/// `check_interval_hours`, until `stop` is set.
fn check_for_updates(config: HostConfig, updates: Updates, build: String, stop: Arc<AtomicBool>) {
    let Some(settings) = config.update.clone() else {
        return;
    };
    let keys = match settings.keys() {
        Ok(keys) => keys,
        Err(error) => return eprintln!("{build}: not checking for updates: {error}"),
    };
    let interval = Duration::from_secs(settings.check_interval_hours.max(1) * 3600);
    let mut next = Instant::now() + Duration::from_secs(60);
    while !stop.load(Ordering::Relaxed) {
        if Instant::now() < next {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        next = Instant::now() + interval;
        match check(
            &updates.store,
            &WinHttpFetch::default(),
            &settings.list_url,
            &keys,
        ) {
            Ok(Some(notice)) => {
                eprintln!("{build}: {notice}");
                updates.notices.set(notice);
            }
            Ok(None) => {}
            Err(error) => eprintln!("{build}: checking for updates: {error}"),
        }
    }
}

/// Serves the local API until `stop` is set, then stops every user's VM.
pub fn serve(config: HostConfig, build: &str, stop: Arc<AtomicBool>) -> Result<(), String> {
    let sddl = config.pipe_sddl.as_deref().unwrap_or(DEFAULT_PIPE_SDDL);
    let listener = NamedPipeListener::bind(&config.pipe_name, sddl)
        .map_err(|e| format!("opening the pipe {:?}: {e}", config.pipe_name))?;
    eprintln!(
        "{build}: serving the local API on \\\\.\\pipe\\{}",
        config.pipe_name
    );
    let grace = Duration::from_secs(config.stop_timeout_secs + 15);
    let updates = open_updates(&config)?;
    if let Some(updates) = &updates {
        let (config, updates, build, stop) = (
            config.clone(),
            updates.clone(),
            build.to_string(),
            Arc::clone(&stop),
        );
        thread::Builder::new()
            .name("dsa-updates".into())
            .spawn(move || check_for_updates(config, updates, build, stop))
            .map_err(|e| format!("starting the update check: {e}"))?;
    }
    let factory_config = config.clone();
    let factory_build = build.to_string();
    let androids = Arc::new(PerUser::new(move |sid: &str| {
        let android = create_user_android(&factory_config, &factory_build, sid, updates.as_ref());
        if let Err(why) = &android {
            eprintln!("{factory_build}: no Android for {sid}: {why}");
        }
        android
    }));
    let service = Arc::new(HostApiService::new(androids.clone(), build));
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
