//! The DSA service: the local API on a named pipe, and one Android VM per user behind it.
//!
//! `dsa-hostd` runs this in the foreground (`serve`) or as a Windows service (`service`).
//! Each caller's SID, as Windows reports it for the pipe connection, picks the VM: the
//! first request of a user creates their data disk and their session, and every later one
//! reaches the same session.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{HostApiService, PerUser, UserAndroid};
use dsa_host_core::session::Session;
use dsa_transport::Listener;

use crate::android_vm::AndroidVmLauncher;
use crate::guid::Guid;
use crate::host_config::HostConfig;
use crate::hvsocket::HvSocketListener;
use crate::pipe::{NamedPipeListener, DEFAULT_PIPE_SDDL};
use crate::user_disk::prepare_user_data_disk;
use crate::vm_listener::VmBoundListener;

/// The user's Android: their data disk, VM and session, created on their first request.
pub fn create_user_android(
    config: &HostConfig,
    build: &str,
    sid: &str,
) -> Result<Arc<dyn UserAndroid>, String> {
    let user = config.user_vm(sid).map_err(|e| e.to_string())?;
    prepare_user_data_disk(&user.config.data_disk, user.data_disk_size_gb)?;
    let launcher = AndroidVmLauncher::new(&user.vm_name, user.config);
    let service = Guid::vsock_service(config.control_port);
    let listener = VmBoundListener::new(
        launcher.vm_id_slot(),
        Box::new(move |vm_id| {
            HvSocketListener::bind(vm_id, service).map(|l| Box::new(l) as Box<dyn Listener>)
        }),
    );
    let session_config = config.session_config();
    let handshake = ControlHandshake::new(build, session_config.connect_timeout);
    Ok(Arc::new(Session::new(
        launcher,
        Box::new(listener),
        handshake,
        session_config,
    )))
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
    let factory_config = config.clone();
    let factory_build = build.to_string();
    let androids = Arc::new(PerUser::new(move |sid: &str| {
        let android = create_user_android(&factory_config, &factory_build, sid);
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
