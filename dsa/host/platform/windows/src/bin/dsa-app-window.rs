//! The app window host of Desktop Subsystem for Android.
//!
//! Each Android app runs its own copy of this program,
//! `%LOCALAPPDATA%\DSA\apps\<package>\<Label>.exe`, which carries the app's name and icon, so
//! Task Manager and the taskbar show the app as itself. It connects to the DSA Window Manager
//! in the same session, opens the app's task
//! windows and shows their frames, and ends when the app's last window closes. Started by the
//! user rather than the window manager (from the app's Start menu shortcut, say), it starts
//! Android and the window manager when they are not running, then asks for the app.
#![windows_subsystem = "windows"]

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-app-window runs on Windows only");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::sync::Arc;

    use dsa_host_windows::app_window::launch::{
        ensure_android, ensure_window_manager, pipe_exists, run_program, tell_user,
        window_manager_path, StartingWindow, POLL, START_WINDOW_MANAGER,
    };
    use dsa_host_windows::app_window::{
        argument, package_of, run, use_app_user_model_id, HostSetup, PACKAGE_ARGUMENT,
        PIPE_ARGUMENT, STARTED_BY_WINDOW_MANAGER,
    };
    use dsa_host_windows::compositor::current_session;
    use dsa_host_windows::compositor::hosts::pipe_name;
    use dsa_host_windows::compositor::log::{default_log_dir, note, CompositorLogger};
    use dsa_host_windows::pipe::{NamedPipeConnector, SameUserPipeConnector};

    let logger = CompositorLogger::open(default_log_dir().join("dsa-app-window.log"))
        .ok()
        .map(Arc::new);
    // Its windows show frames and take touches whether or not one has the focus.
    if let Err(why) = dsa_host_windows::scheduling::raise_this_process() {
        note(logger.as_deref(), &why);
    }
    dsa_host_windows::compositor::log::input_trace_to(logger.clone());
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
    // The app's own ID, which its Start menu shortcut carries too, so the taskbar shows the
    // app on a button of its own.
    if let Err(why) = use_app_user_model_id(&package) {
        note(logger.as_deref(), &why);
    }
    let launch = !args.iter().any(|arg| arg == STARTED_BY_WINDOW_MANAGER);
    let window_manager_pipe =
        argument(&args, PIPE_ARGUMENT).unwrap_or_else(|| pipe_name(current_session()));
    let api_pipe = dsa_host_windows::host_config::DEFAULT_PIPE_NAME;
    let title = program
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| package.clone());

    // Opened by the user (from the Start menu, a pinned button or Windows search): Android
    // and the window manager are started first when they are not running.
    let mut starting = None;
    if launch {
        let prepared = ensure_android(
            &mut NamedPipeConnector {
                name: api_pipe.into(),
            },
            || starting = Some(StartingWindow::show(&title)),
        )
        .and_then(|()| {
            ensure_window_manager(
                || pipe_exists(&window_manager_pipe),
                || match window_manager_path(Some(program.as_path())) {
                    Some(window_manager) => run_program(&window_manager, ""),
                    None => Err("the DSA Window Manager is not installed".into()),
                },
                START_WINDOW_MANAGER,
                POLL,
            )
        });
        if let Err(why) = prepared {
            note(logger.as_deref(), &format!("{package}: {why}"));
            drop(starting);
            tell_user(&title, &format!("{title} could not be opened. {why}."));
            return std::process::ExitCode::FAILURE;
        }
    }

    let listing: Arc<std::sync::Mutex<Vec<_>>> = Arc::default();
    if let Some(window) = starting {
        // The waiting window stays until the app's first window opens, so the taskbar never
        // loses the app in between.
        let opened = Arc::clone(&listing);
        let _ = std::thread::Builder::new()
            .name("dsa-app-window-waiting".into())
            .spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while opened.lock().map(|open| open.is_empty()).unwrap_or(false)
                    && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                window.close();
            });
    }

    let setup = HostSetup {
        package: package.clone(),
        launch,
        connector: Box::new(SameUserPipeConnector {
            name: window_manager_pipe,
        }),
        input: Box::new(NamedPipeConnector {
            name: api_pipe.into(),
        }),
        process_id: std::process::id(),
        logger: logger.clone(),
        listing,
    };
    match run(setup) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(why) => {
            note(logger.as_deref(), &format!("{package}: {why}"));
            std::process::ExitCode::FAILURE
        }
    }
}
