//! The Desktop Subsystem for Android user-session compositor for Windows.
//!
//! Runs in the user session without a console window, accepts guest graphics streams
//! on Hyper-V sockets (ports 5566 and 5567), shows one window per Android task with that
//! task's frames, reconnects when the VM restarts, and exits cleanly on sign-out.
#![windows_subsystem = "windows"]

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-compositor runs on Windows only");
    std::process::ExitCode::FAILURE
}

#[cfg(windows)]
fn main() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use dsa_host_windows::compositor::adapter::GfxstreamAdapter;
    use dsa_host_windows::compositor::log::{default_log_file, CompositorLogger};
    use dsa_host_windows::compositor::shutdown::SignOutListener;
    use dsa_host_windows::compositor::single_instance::SingleInstanceMutex;
    use dsa_host_windows::compositor::{Compositor, CompositorConfig};

    let log_file = default_log_file();
    let logger = CompositorLogger::open(&log_file).ok().map(Arc::new);

    let config = CompositorConfig::default();

    // Enforce single instance per user session.
    let _instance_guard = match SingleInstanceMutex::acquire(&config.mutex_name) {
        Ok(guard) => guard,
        Err(why) => {
            if let Some(log) = &logger {
                log.log(&format!("exiting: {why}"));
            }
            return;
        }
    };

    if let Some(log) = &logger {
        log.log(&format!(
            "dsa-compositor starting (version {})",
            dsa_host_windows::VERSION
        ));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let _signout = SignOutListener::start(Arc::clone(&stop));

    let renderer = Arc::new(GfxstreamAdapter::new(None));
    let mut compositor = Compositor::new(config, renderer.clone()).with_display_presenter(renderer);
    if let Some(log) = logger {
        compositor = compositor.with_logger(log);
    }

    compositor.run(stop);
}
