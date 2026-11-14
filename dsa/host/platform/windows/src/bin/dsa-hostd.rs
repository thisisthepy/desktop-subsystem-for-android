//! dsa-hostd: runs the Android VMs and their control channels.
//!
//! ```text
//! dsa-hostd <config.json>          one VM, driven from standard input
//! dsa-hostd serve <config.json>    the local API in the foreground, one VM per user
//! dsa-hostd service <config.json>  the same, as a Windows service
//! dsa-hostd remove-images <folder> removes the downloaded images (the uninstaller runs it)
//! dsa-hostd remove-updates <folder> removes the downloaded setup files (the same)
//! dsa-hostd remove-machine-data <folder> removes the settings, logs and tmp folder (the same)
//! dsa-hostd remove-user-data       removes every user's shortcuts, files and keys DSA made,
//!                                  keeping their Android disk (the same)
//! dsa-hostd stop-programs <folder> ends every DSA program running from the program folder or
//!                                  from a user's app windows, in every session (the
//!                                  installer runs it before it replaces the files)
//! dsa-hostd start-window-managers  starts the window manager beside this program for every
//!                                  signed-in user (the installer runs it at the end)
//! dsa-hostd hold-service           keeps the service from being started (the installer
//!                                  runs it before it stops the service)
//! dsa-hostd release-service [--start]
//!                                  lets the service start again and stops one started from
//!                                  the old files (the installer runs it once the new files
//!                                  are in place; with --start, when it rolls back)
//! dsa-hostd restart-service        waits for the service to stop, ends the programs left
//!                                  from the old files, and starts the service and the window
//!                                  managers again (the service runs it to restart into files
//!                                  setup replaced under it)
//! dsa-hostd install-image <folder> [<config.json>]
//!                                  installs the Android image in <folder> (a signed
//!                                  images.json and its files) through the update checks, for
//!                                  `dsa android install-image`; the configuration is the
//!                                  installed one unless named
//! dsa-hostd check-setup <folder> [<config.json>]
//!                                  checks the setup file in <folder> against the signed
//!                                  program.json beside it, as an update is checked, and
//!                                  prints its version and path (the development build's
//!                                  install script runs it before setup)
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

    use dsa_host_core::guest_display::{
        display_or_smallest, with_display_density, with_display_size,
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
                         dsa-hostd remove-images <folder> | dsa-hostd remove-updates <folder> | \
                         dsa-hostd remove-machine-data <folder> | dsa-hostd remove-user-data | \
                         dsa-hostd stop-programs <folder> | dsa-hostd start-window-managers | \
                         dsa-hostd hold-service | dsa-hostd release-service [--start] | \
                         dsa-hostd restart-service | \
                         dsa-hostd install-image <folder> [<config.json>] | \
                         dsa-hostd check-setup <folder> [<config.json>]";

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
        let mut config = HostConfig::parse(&expand_locations(&text, &data, &program))
            .map_err(|e| e.to_string())?;
        // Every installation records what its package carries beside the configuration,
        // which upgrades keep as the user left it.
        let package_path = data.join(dsa_host_windows::host_config::INSTALLED_PACKAGE_FILE);
        match std::fs::read_to_string(&package_path) {
            Ok(text) => match dsa_host_windows::host_config::InstalledPackage::parse(
                &expand_locations(&text, &data, &program),
            ) {
                Ok(package) => {
                    config.apply_package(&package);
                }
                Err(error) => config
                    .package_notes
                    .push(format!("ignoring {}: {error}", package_path.display())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => config
                .package_notes
                .push(format!("reading {}: {error}", package_path.display())),
        }
        config.machine_root = Some(data);
        Ok(config)
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
            [mode, path] if mode == "remove-machine-data" => {
                use dsa_host_windows::paths::MachineRoot;
                use dsa_host_windows::uninstall::{installer_folder, remove_machine_files};
                for why in remove_machine_files(&MachineRoot::at(installer_folder(path))) {
                    eprintln!("{why}");
                }
                Ok(())
            }
            // remove-start-menu is the name earlier packages used.
            [mode] if mode == "remove-user-data" || mode == "remove-start-menu" => {
                remove_every_users_data();
                Ok(())
            }
            [mode, path] if mode == "stop-programs" => {
                use dsa_host_windows::installer_actions::stop_dsa_programs;
                use dsa_host_windows::uninstall::installer_folder;
                for why in stop_dsa_programs(&installer_folder(path)) {
                    eprintln!("{why}");
                }
                Ok(())
            }
            [mode] if mode == "start-window-managers" => {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                let program = exe.with_file_name("dsa-wm.exe");
                for why in dsa_host_windows::window_manager::start_for_signed_in_users(&program) {
                    eprintln!("{why}");
                }
                Ok(())
            }
            [mode] if mode == "hold-service" => hold_service(),
            [mode] if mode == "release-service" => release_service(false),
            [mode, start] if mode == "release-service" && start == "--start" => {
                release_service(true)
            }
            [mode] if mode == "restart-service" => restart_service(),
            [mode, folder] if mode == "check-setup" => check_setup(folder, None),
            [mode, folder, config] if mode == "check-setup" => check_setup(folder, Some(config)),
            [mode, folder] if mode == "install-image" => install_image(folder, None),
            [mode, folder, config] if mode == "install-image" => {
                install_image(folder, Some(config))
            }
            [path] if path != "serve" && path != "service" => one_vm(read_config(path)?),
            _ => Err(USAGE.into()),
        }
    }

    /// The installed configuration: `dsa-hostd.json` in the data folder the installer
    /// recorded, or in `%ProgramData%\DSA` when it recorded none.
    fn installed_config() -> std::path::PathBuf {
        use dsa_host_windows::paths::MachineRoot;
        match installed_data_folder() {
            Some(folder) => MachineRoot::at(folder).config_file(),
            None => MachineRoot::default_location().config_file(),
        }
    }

    /// The data folder the installer recorded in the registry.
    fn installed_data_folder() -> Option<std::path::PathBuf> {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
        };
        let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let key = wide(r"SOFTWARE\thisisthepy\DSA");
        let value = wide("DataFolder");
        let mut buffer = vec![0u16; 1024];
        let mut size = (buffer.len() * 2) as u32;
        // SAFETY: NUL-terminated names and a buffer of the size given.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..end]);
        let text = text.trim().trim_end_matches(['\\', '/']);
        (!text.is_empty()).then(|| std::path::PathBuf::from(text))
    }

    /// The keys the installed configuration (or the one named) trusts for the release lists.
    fn trusted_keys(
        config: Option<&String>,
    ) -> Result<(HostConfig, Vec<ed25519_dalek::VerifyingKey>), String> {
        let config_path = config
            .map(std::path::PathBuf::from)
            .unwrap_or_else(installed_config);
        let config = read_config(&config_path.to_string_lossy())?;
        let keys = config
            .update
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "{} trusts no release key (it has no update settings), so nothing can be \
                     checked",
                    config_path.display()
                )
            })?
            .keys()
            .map_err(|e| e.to_string())?;
        Ok((config, keys))
    }

    /// Checks the setup file in `folder` against the signed program list beside it, as the
    /// service checks an update of DSA it downloaded, and prints its version and path.
    fn check_setup(folder: &str, config: Option<&String>) -> Result<(), String> {
        let (_, keys) = trusted_keys(config)?;
        let (list, path) = dsa_host_core::update::local::check_setup_in_folder(
            std::path::Path::new(folder),
            &keys,
        )
        .map_err(|e| e.to_string())?;
        println!("{} {}", list.version, path.display());
        Ok(())
    }

    /// Installs the Android image in `folder` into the image store the service boots from,
    /// trusting the keys the service trusts: the same signature, rules and hash checks as a
    /// downloaded update. The service picks it up as it does a downloaded image: at the next
    /// start of Android, or by restarting a running Android once no app window is open.
    fn install_image(folder: &str, config: Option<&String>) -> Result<(), String> {
        use dsa_host_core::update::local::install_from_folder;
        use dsa_host_core::update::store::ImageStore;
        let (config, keys) = trusted_keys(config)?;
        let (Some(root), Some(configured)) = (&config.images_dir, &config.first_image) else {
            return Err(
                "the configuration names no image store (images_dir and first_image), so \
                 there is nothing to install into"
                    .into(),
            );
        };
        let first = config.bundled_image.as_ref().unwrap_or(configured);
        let store = ImageStore::open_as(root, first, &build()).map_err(|e| e.to_string())?;
        let folder = std::path::Path::new(folder);
        let mut shown = None;
        let image = install_from_folder(&store, folder, &keys, &mut |copy| {
            let percent = (copy.done as u128 * 100)
                .checked_div(copy.total as u128)
                .unwrap_or(100);
            if shown != Some(percent / 10) {
                shown = Some(percent / 10);
                eprintln!("copying Android {}: {percent}%", copy.image.version);
            }
        })
        .map_err(|e| e.to_string())?;
        println!(
            "Android {} is installed and boots on trial at the next start of Android; a \
             running Android restarts into it once no app window is open (dsa android restart \
             restarts it now). If it does not finish booting, the service goes back to the \
             image before it.",
            image.version
        );
        Ok(())
    }

    /// The service the installer registers.
    const SERVICE: &str = "DsaHost";
    /// How long stopping the service may take: every user's Android stops first.
    const SERVICE_STOP_WAIT: Duration = Duration::from_secs(300);

    /// Before setup stops the service to replace it: holds it so nothing starts it again
    /// meanwhile. The `dsa` command starts a stopped service when a client cannot reach it,
    /// and the settings app runs it every few seconds, so the old service came back the moment
    /// setup stopped it and kept its files in use.
    fn hold_service() -> Result<(), String> {
        match scm::set_start_type(SERVICE, scm::StartType::Disabled) {
            Ok(()) => Ok(()),
            // A first installation has no service yet.
            Err(why) => {
                eprintln!("{why}");
                Ok(())
            }
        }
    }

    /// Once setup has put the new files in place (or, with `start`, when setup is rolled
    /// back): the service starts with Windows again. A service that is running now was
    /// started from the old files after setup stopped it, so it is stopped, for setup to start
    /// the new one; with `start` the service is started here, as setup's rollback cannot while
    /// it is held.
    fn release_service(start: bool) -> Result<(), String> {
        if let Err(why) = scm::set_start_type(SERVICE, scm::StartType::Automatic) {
            eprintln!("{why}");
            return Ok(());
        }
        if start {
            if let Err(why) = scm::WindowsServiceStarter::new(SERVICE, SERVICE_STOP_WAIT).start() {
                eprintln!("{why}");
            }
            return Ok(());
        }
        match scm::stop_and_wait(SERVICE, SERVICE_STOP_WAIT) {
            Ok(true) => eprintln!(
                "the service was running from the old files while setup replaced them; it was \
                 stopped, so the new one starts"
            ),
            Ok(false) => {}
            Err(why) => eprintln!("{why}"),
        }
        Ok(())
    }

    /// Restarts the service into the program files installed now, as a service that setup
    /// installed past asks once it has stopped: waits for it to stop, ends every DSA program
    /// still running from the old files in every session (the window managers, their
    /// renderers, the app windows, the settings app; each speaks the old local API and would
    /// be refused), starts the service, and starts every signed-in user's window manager from
    /// the new files.
    fn restart_service() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let folder = exe
            .parent()
            .map(std::path::Path::to_path_buf)
            .ok_or("this program is in no folder")?;
        scm::wait_until_stopped(SERVICE, SERVICE_STOP_WAIT)?;
        for why in dsa_host_windows::installer_actions::stop_dsa_programs(&folder) {
            eprintln!("{why}");
        }
        scm::WindowsServiceStarter::new(SERVICE, Duration::from_secs(60)).start()?;
        for why in
            dsa_host_windows::window_manager::start_for_signed_in_users(&folder.join("dsa-wm.exe"))
        {
            eprintln!("{why}");
        }
        Ok(())
    }

    /// Removes, for every user with a profile, the Start menu shortcuts, files and registry
    /// keys DSA made for them, keeping their Android disk. A user whose local application data
    /// was moved is found where it went, and also where it would be by default.
    fn remove_every_users_data() {
        use dsa_host_windows::paths::{
            default_local_app_data, start_menu_programs_in_profile, UserRoot,
        };
        use dsa_host_windows::uninstall::remove_user_files;
        for (sid, profile) in dsa_host_windows::profile::profiles() {
            let programs = start_menu_programs_in_profile(&profile);
            let default_local = default_local_app_data(&profile);
            let local = dsa_host_windows::profile::local_app_data(&sid)
                .unwrap_or_else(|_| default_local.clone());
            let mut roots = vec![UserRoot::in_local_app_data(&local)];
            if local != default_local {
                roots.push(UserRoot::in_profile(&profile));
            }
            for root in &roots {
                for why in remove_user_files(root, &programs) {
                    eprintln!("{sid}: {why}");
                }
            }
            for why in dsa_host_windows::user_registry::remove_user_keys(&sid, &local) {
                eprintln!("{sid}: {why}");
            }
            for why in dsa_host_windows::installed_apps::remove_for_user(&sid, &profile) {
                eprintln!("{why}");
            }
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
        let mut vm = config
            .vm_config(fallback_sid.as_deref())
            .map_err(|e| e.to_string())?;
        // Run by hand, this process is in the user's session and sees their monitors.
        let monitors = dsa_host_windows::monitors::current();
        let size = display_or_smallest(Some(&monitors));
        vm.kernel_cmdline = with_display_size(&vm.kernel_cmdline, size);
        let density = dsa_host_windows::monitors::system_density();
        vm.kernel_cmdline = with_display_density(&vm.kernel_cmdline, density);
        let user = config.user_sid.clone().or(fallback_sid).unwrap_or_default();
        let console_log = config.console_log_path(&user, &vm.data_disk);
        println!("the Android console is kept in {}", console_log.display());
        let traces_input = dsa_host_windows::service::guest_traces_input(&vm.full_kernel_cmdline());
        let launcher = AndroidVmLauncher::new(&config.vm_name, vm).with_console_log(console_log);
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
        let session_config = config.session_config();
        let build = build();
        let handshake = ControlHandshake::new(&build, session_config.connect_timeout);
        let session = Arc::new(Session::new(
            launcher,
            Box::new(listener),
            handshake,
            session_config,
        ));
        // Android's log, read with `dsa logcat` through the local API when the service runs it;
        // here the port is served so the guest's daemon finds the host there.
        session.serve_log_port(Box::new(log_listener), &build);
        session.set_clock_trace(Some(dsa_host_core::clock_trace::ClockTrace::new(
            Arc::new(AtomicBool::new(traces_input)),
            Arc::new(dsa_host_windows::compositor::log::PerformanceCounterClock),
            Arc::new(|line: &str| eprintln!("{line}")),
        )));
        // The guest's battery follows the computer's power.
        let power = Arc::new(PowerHub::new());
        power.attach(&session);
        // The guest's display refreshes as fast as the fastest monitor, which this process sees
        // itself; energy saver, which comes with the power, caps it at 60 Hz.
        {
            let watched = Arc::downgrade(&session);
            thread::spawn(move || {
                dsa_host_core::guest_refresh::follow_monitors(
                    watched,
                    dsa_host_windows::monitors::current_refresh,
                    dsa_host_core::guest_refresh::MONITOR_POLL,
                )
            });
        }
        let power_stop = Arc::new(AtomicBool::new(false));
        {
            let (power, stop) = (Arc::clone(&power), Arc::clone(&power_stop));
            thread::spawn(move || dsa_host_windows::power::follow(power, stop));
        }
        // And the computer's sleep, for as long as this runs.
        let _sleep_watch = dsa_host_windows::power::watch_sleep(Arc::clone(&power))
            .map_err(|why| eprintln!("Android will not hear when the computer sleeps: {why}"))
            .ok();

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
