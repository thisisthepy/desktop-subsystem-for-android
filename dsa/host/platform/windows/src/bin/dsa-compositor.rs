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
    use dsa_host_windows::compositor::display_gpu::{adapter_names, display_sources};
    use dsa_host_windows::compositor::log::{default_log_file, CompositorLogger};
    use dsa_host_windows::compositor::shutdown::SignOutListener;
    use dsa_host_windows::compositor::single_instance::SingleInstanceMutex;
    use dsa_host_windows::compositor::{Compositor, CompositorConfig};
    use dsa_host_windows::render_gpu::{
        choose, default_settings_file, describe_outcome, describe_request, CompositorSettings,
    };

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

    // The GPU the renderer runs on: the one driving the primary monitor unless the user's
    // settings name another. It is chosen once, before the renderer starts.
    let log = |message: &str| {
        if let Some(log) = &logger {
            log.log(message);
        }
    };
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    let (settings, settings_problem) =
        CompositorSettings::load(&default_settings_file(local_app_data.as_deref()));
    if let Some(why) = settings_problem {
        log(&why);
    }
    let sources = display_sources().unwrap_or_else(|why| {
        log(&why);
        Vec::new()
    });
    let request = choose(&settings.render_gpu, &sources);
    log(&describe_request(
        &settings.render_gpu,
        &request,
        &adapter_names(),
    ));

    let renderer = Arc::new(GfxstreamAdapter::on_gpu(None, &request));
    for note in renderer.notes() {
        log(note);
    }
    match renderer.renderer_gpu() {
        Some(gpu) => log(&describe_outcome(&request, &gpu)),
        None if renderer.is_available() => {
            log("the graphics backend does not say which GPU it renders on")
        }
        None => {}
    }
    let mut compositor = Compositor::new(config, renderer.clone()).with_display_presenter(renderer);
    if let Some(log) = logger {
        compositor = compositor.with_logger(log);
    }

    compositor.run(stop);
}
