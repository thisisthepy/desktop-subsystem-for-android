//! The app window host of Desktop Subsystem for Android.
//!
//! Each Android app runs its own copy of this program, `%LOCALAPPDATA%\DSA\apps\<package>.exe`,
//! which carries the app's name and icon, so Task Manager and the taskbar show the app as
//! itself. It connects to the DSA Window Manager in the same session, opens the app's task
//! windows and shows their frames, and ends when the app's last window closes. Started by the
//! user rather than the window manager, it asks for the app to be started.
#![windows_subsystem = "windows"]

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-app-window runs on Windows only");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::sync::Arc;

    use dsa_host_windows::app_window::{
        argument, package_of, run, HostSetup, PACKAGE_ARGUMENT, PIPE_ARGUMENT,
        STARTED_BY_WINDOW_MANAGER,
    };
    use dsa_host_windows::compositor::current_session;
    use dsa_host_windows::compositor::hosts::pipe_name;
    use dsa_host_windows::compositor::log::{default_log_dir, note, CompositorLogger};
    use dsa_host_windows::pipe::SameUserPipeConnector;

    let logger = CompositorLogger::open(default_log_dir().join("dsa-app-window.log"))
        .ok()
        .map(Arc::new);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let program = std::env::current_exe().unwrap_or_default();
    let Some(package) = package_of(&program, &args) else {
        note(
            logger.as_deref(),
            &format!(
                "{} names no Android app; name one with {PACKAGE_ARGUMENT}",
                program.display()
            ),
        );
        return std::process::ExitCode::FAILURE;
    };
    let setup = HostSetup {
        package: package.clone(),
        launch: !args.iter().any(|arg| arg == STARTED_BY_WINDOW_MANAGER),
        connector: Box::new(SameUserPipeConnector {
            name: argument(&args, PIPE_ARGUMENT).unwrap_or_else(|| pipe_name(current_session())),
        }),
        process_id: std::process::id(),
        logger: logger.clone(),
        listing: Arc::default(),
    };
    match run(setup) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(why) => {
            note(logger.as_deref(), &format!("{package}: {why}"));
            std::process::ExitCode::FAILURE
        }
    }
}
