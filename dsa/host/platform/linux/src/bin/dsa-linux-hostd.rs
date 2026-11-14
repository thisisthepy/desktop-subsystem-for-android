//! dsa-linux-hostd: the DSA service on Linux. Its name differs from the other hosts' service
//! (`dsa-hostd`) because every host is built in one workspace, where two programs of one name
//! would overwrite each other.
//!
//! ```text
//! dsa-linux-hostd [serve | --config] [<file>]
//! ```
//!
//! Serves the local API on the configured socket (`/run/dsa/host.sock`) until it is told to
//! stop (SIGTERM or SIGINT, as systemd and a terminal send), then stops every user's Android.
//! The configuration file is `/etc/dsa/dsa-hostd.json` unless `--config` names another.

use std::process::ExitCode;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use dsa_host_linux::config::{HostConfig, DEFAULT_CONFIG};

    static SIGNALLED: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_: libc::c_int) {
        SIGNALLED.store(true, Ordering::SeqCst);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    // The service starts this program again as a user, to reach that user's Secret Service.
    if let [helper] = args.as_slice() {
        if helper == dsa_host_linux::secret_service::HELPER_ARG {
            return dsa_host_linux::secret_service::helper_main();
        }
    }
    let path = match args.as_slice() {
        [] => DEFAULT_CONFIG.to_string(),
        [serve] if serve == "serve" => DEFAULT_CONFIG.to_string(),
        [flag, path] if flag == "--config" || flag == "serve" => path.clone(),
        _ => {
            eprintln!("usage: dsa-linux-hostd [serve | --config] [<file>]");
            return ExitCode::from(2);
        }
    };
    let config = match HostConfig::load(std::path::Path::new(&path)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("dsa-linux-hostd: {error}");
            return ExitCode::FAILURE;
        }
    };
    let handler: extern "C" fn(libc::c_int) = on_signal;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        // A client that goes away mid-answer is an error to report, not a reason to die.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || loop {
            if SIGNALLED.load(Ordering::SeqCst) {
                stop.store(true, Ordering::SeqCst);
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        });
    }
    let build = format!("dsa-linux-hostd {}", dsa_host_linux::VERSION);
    match dsa_host_linux::service::serve(config, &build, stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dsa-linux-hostd: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("dsa-linux-hostd is the Linux host's service; this system has its own");
    ExitCode::FAILURE
}
