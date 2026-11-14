//! `dsa-wm` on macOS: the window manager of Desktop Subsystem for Android.
//!
//! Runs as the user, from its launchd agent (installed by `dsa-hostd install-agent` beside
//! the service's own) or started by `dsa-hostd` when there is no agent. It starts the graphics
//! renderer (`dsa-gfx`), which serves the guest's graphics streams, shows each Android task
//! in a window of the app's own window host, shows the widgets placed on the desktop as panels
//! of its own (its main thread runs AppKit for them), and exits cleanly on SIGTERM, which
//! launchd sends at logout.
//!
//! `DSA_RENDERER` names the renderer library to load instead of the one in DSA.app.

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-wm: this build is the macOS window manager and runs on macOS only");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    use dsa_host_macos::wm::run::{run, Places};

    static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    extern "C" fn on_signal(_: libc::c_int) {
        if let Some(stop) = STOP.get() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    if std::env::args().any(|arg| arg == "version") {
        println!("dsa-wm {}", dsa_host_macos::VERSION);
        return std::process::ExitCode::SUCCESS;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("dsa-wm: HOME is not set, so DSA's folder cannot be found");
        return std::process::ExitCode::FAILURE;
    };
    let stop = Arc::new(AtomicBool::new(false));
    let _ = STOP.set(Arc::clone(&stop));
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        // A window host that goes away mid-write must not end the window manager.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    match run(Places::for_home(&home), stop) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("dsa-wm: {why}");
            std::process::ExitCode::FAILURE
        }
    }
}
