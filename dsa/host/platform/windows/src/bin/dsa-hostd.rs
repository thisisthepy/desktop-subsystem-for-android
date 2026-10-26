//! dsa-hostd: runs the Android VMs and their control channels.
//!
//! ```text
//! dsa-hostd <config.json>          one VM, driven from standard input
//! dsa-hostd serve <config.json>    the local API in the foreground, one VM per user
//! dsa-hostd service <config.json>  the same, as a Windows service
//! dsa-hostd remove-images <folder> removes the downloaded images (the uninstaller runs it)
//! dsa-hostd remove-updates <folder> removes the downloaded setup files (the same)
//! ```
//!
//! Without a mode it starts one VM, waits for the guest's channel daemon to connect and
//! pass the protocol handshake, and prints every state change. Commands on standard input:
//! `start`, `stop`, `freeze`, `status`, `quit`. End of input also quits; quitting stops the
//! VM first.
//!
//! `serve` and `service` answer the local API on a named pipe and run each caller's own
//! VM. `serve` stops on Ctrl+C. `service` is what the installer registers; it writes its
//! messages to `log_file`, since a service has no console. Both stop every VM before they
//! exit.
//!
//! Creating a VM needs administrator rights, so run it from an elevated prompt or as the
//! service (which runs as SYSTEM).

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-hostd runs on Windows only");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match windows_host::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dsa-hostd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod windows_host {
    use std::io::BufRead;
    use std::os::windows::io::IntoRawHandle;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::thread;
    use std::time::Duration;

    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, SetStdHandle, STD_ERROR_HANDLE,
    };

    use dsa_host_core::handshake::ControlHandshake;
    use dsa_host_core::power::PowerHub;
    use dsa_host_core::session::{Session, SessionState};
    use dsa_host_windows::account::current_user_sid;
    use dsa_host_windows::android_vm::AndroidVmLauncher;
    use dsa_host_windows::guid::Guid;
    use dsa_host_windows::host_config::{expand_locations, HostConfig};
    use dsa_host_windows::hvsocket::HvSocketListener;
    use dsa_host_windows::vm_listener::VmBoundListener;
    use dsa_host_windows::{scm, service};
    use dsa_transport::Listener;

    const USAGE: &str = "usage: dsa-hostd [serve | service] <config.json> | \
                         dsa-hostd remove-images <folder> | dsa-hostd remove-updates <folder>";

    fn build() -> String {
        format!("dsa-hostd {}", dsa_host_windows::VERSION)
    }

    fn read_config(path: &str) -> Result<HostConfig, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
        // The installer lets the user choose both folders, so the file names them as
        // {data} (the file's own folder) and {program} (this program's folder).
        let data = std::path::Path::new(path)
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let program = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
            .unwrap_or_default();
        HostConfig::parse(&expand_locations(&text, &data, &program)).map_err(|e| e.to_string())
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.as_slice() {
            [mode, path] if mode == "serve" => serve_in_foreground(read_config(path)?),
            [mode, path] if mode == "service" => run_service(path.clone()),
            [mode, path] if mode == "remove-images" => {
                use dsa_host_windows::uninstall::{installer_folder, remove_downloaded_images};
                remove_downloaded_images(&installer_folder(path))
            }
            [mode, path] if mode == "remove-updates" => {
                use dsa_host_windows::uninstall::{installer_folder, remove_downloaded_updates};
                remove_downloaded_updates(&installer_folder(path))
            }
            [path] if path != "serve" && path != "service" => one_vm(read_config(path)?),
            _ => Err(USAGE.into()),
        }
    }

    static CONSOLE_STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    unsafe extern "system" fn on_console_control(_kind: u32) -> BOOL {
        if let Some(stop) = CONSOLE_STOP.get() {
            stop.store(true, Ordering::Relaxed);
        }
        1
    }

    fn serve_in_foreground(config: HostConfig) -> Result<(), String> {
        let stop = Arc::new(AtomicBool::new(false));
        let _ = CONSOLE_STOP.set(Arc::clone(&stop));
        // SAFETY: a handler with the right signature, installed once.
        unsafe { SetConsoleCtrlHandler(Some(on_console_control), 1) };
        service::serve(config, &build(), stop)
    }

    fn run_service(path: String) -> Result<(), String> {
        scm::run_as_service(Box::new(move |stop| {
            let config = read_config(&path)?;
            if let Some(log) = &config.log_file {
                redirect_stderr(log)?;
            }
            service::serve(config, &build(), stop)
        }))
    }

    /// Sends everything written to standard error to the end of `path`.
    fn redirect_stderr(path: &std::path::Path) -> Result<(), String> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|e| format!("creating {}: {e}", folder.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("opening the log {}: {e}", path.display()))?;
        // The handle stays open for the life of the process.
        let handle = file.into_raw_handle();
        // SAFETY: a valid file handle that is never closed.
        if unsafe { SetStdHandle(STD_ERROR_HANDLE, handle as HANDLE) } == 0 {
            return Err(format!(
                "writing the log to {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn one_vm(config: HostConfig) -> Result<(), String> {
        let fallback_sid = if config.user_sid.is_none() {
            Some(current_user_sid()?)
        } else {
            None
        };
        let vm = config
            .vm_config(fallback_sid.as_deref())
            .map_err(|e| e.to_string())?;
        let user = config.user_sid.clone().or(fallback_sid).unwrap_or_default();
        let console_log = config.console_log_path(&user, &vm.data_disk);
        println!("the Android console is kept in {}", console_log.display());
        let launcher = AndroidVmLauncher::new(&config.vm_name, vm).with_console_log(console_log);
        let service = Guid::vsock_service(config.control_port);
        let listener = VmBoundListener::new(
            launcher.vm_id_slot(),
            Box::new(move |vm_id| {
                HvSocketListener::bind(vm_id, service).map(|l| Box::new(l) as Box<dyn Listener>)
            }),
        );
        let session_config = config.session_config();
        let build = build();
        let handshake = ControlHandshake::new(&build, session_config.connect_timeout);
        let session = Arc::new(Session::new(
            launcher,
            Box::new(listener),
            handshake,
            session_config,
        ));
        // The guest's battery follows the computer's power.
        let power = Arc::new(PowerHub::new());
        power.attach(&session);
        let power_stop = Arc::new(AtomicBool::new(false));
        {
            let (power, stop) = (Arc::clone(&power), Arc::clone(&power_stop));
            thread::spawn(move || dsa_host_windows::power::follow(power, stop));
        }

        let events = session.events();
        thread::spawn(move || {
            for event in events {
                match event.reason {
                    Some(reason) => println!("state: {} ({reason})", event.state),
                    None => println!("state: {}", event.state),
                }
            }
        });

        println!(
            "{build}: starting the Android VM {:?}; commands: start, stop, freeze, status, quit",
            config.vm_name
        );
        session.start();
        for line in std::io::stdin().lock().lines() {
            match line.map_err(|e| e.to_string())?.trim() {
                "start" => session.start(),
                "stop" => session.stop(),
                "freeze" => {
                    if let Err(why) = session.freeze() {
                        println!("not frozen: {why}");
                    }
                }
                "status" => match session.last_reason() {
                    Some(reason) => println!("status: {} (last: {reason})", session.state()),
                    None => println!("status: {}", session.state()),
                },
                "quit" => break,
                "" => {}
                other => println!(
                    "unknown command {other:?}; commands: start, stop, freeze, status, quit"
                ),
            }
        }

        power_stop.store(true, Ordering::Relaxed);
        session.stop();
        let grace = Duration::from_secs(config.stop_timeout_secs + 15);
        if !session.wait_for(SessionState::Stopped, grace) {
            return Err(format!(
                "the Android VM did not stop: {}",
                session
                    .last_reason()
                    .map(|r| r.to_string())
                    .unwrap_or_default()
            ));
        }
        Ok(())
    }
}
