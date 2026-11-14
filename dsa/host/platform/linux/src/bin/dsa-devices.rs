//! `dsa-devices`: lends the user's devices to their Android, from inside the user's desktop
//! session.
//!
//! Sound, the cameras, the location, the sensors, printing, web links and the desktop's
//! settings all need the session: its sound server, its portals, its location agent, its
//! browser. The service runs outside it, so this program runs as the user, started with the
//! session by the `dsa-devices.service` user unit, and serves the guest's device ports (which
//! the service relays from the user's VM to sockets in `$XDG_RUNTIME_DIR/dsa/devices`) and
//! follows the local API at `/run/dsa/host.sock` (or `--socket <path>`).
//!
//! It runs until it is stopped (SIGTERM or SIGINT), and logs to standard error, which the
//! user's journal keeps.

#[cfg(target_os = "linux")]
fn main() {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use dsa_host_linux::devices::{self, ConnectorFactory, Followers, Log};
    use dsa_host_unix::socket::UnixLocalConnector;

    let mut socket = PathBuf::from("/run/dsa/host.sock");
    let mut which = Followers::default();
    let mut ports = true;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => match args.next() {
                Some(path) => socket = PathBuf::from(path),
                None => usage("--socket needs a path"),
            },
            "--no-location" => which.location = false,
            "--no-print" => which.print = false,
            "--no-links" => which.links = false,
            "--no-settings" => which.settings = false,
            "--no-language" => which.language = false,
            "--no-geocoding" => which.geocoding = false,
            "--no-share" => which.share = false,
            "--no-device-ports" => ports = false,
            "--version" => {
                println!("dsa-devices {}", dsa_host_linux::VERSION);
                return;
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return;
            }
            other => usage(&format!("unknown argument {other:?}")),
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let stop = Arc::clone(&stop);
        // SAFETY: the handler only stores to an atomic, which is signal safe.
        if let Err(error) = unsafe { register(signal, stop) } {
            eprintln!("dsa-devices: cannot handle signal {signal}: {error}");
        }
    }

    let log: Log = Arc::new(|line: &str| eprintln!("{line}"));
    let connect: ConnectorFactory = Arc::new(move || {
        Box::new(UnixLocalConnector {
            path: socket.clone(),
        }) as Box<dyn dsa_transport::Connector>
    });
    let mut threads = Vec::new();
    if ports {
        threads.extend(devices::start_guest_ports(&connect, &stop, &log));
    }
    threads.extend(devices::start_followers(connect, which, &stop, &log));
    threads.extend(devices::start_monitor_refresh(&stop, &log));
    eprintln!("dsa-devices {}: running", dsa_host_linux::VERSION);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    eprintln!("dsa-devices: stopping");
    for thread in threads {
        let _ = thread.join();
    }
}

#[cfg(target_os = "linux")]
const HELP: &str = "usage: dsa-devices [--socket <path>] [--no-location] [--no-print] \
                    [--no-links] [--no-settings] [--no-language] [--no-geocoding] [--no-share] \
                    [--no-device-ports]";

#[cfg(target_os = "linux")]
fn usage(why: &str) -> ! {
    eprintln!("dsa-devices: {why}\n{HELP}");
    std::process::exit(2);
}

/// The flag each handled signal sets.
#[cfg(target_os = "linux")]
static STOP: std::sync::OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "linux")]
extern "C" fn on_signal(_: libc::c_int) {
    if let Some(stop) = STOP.get() {
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Sets `stop` when `signal` comes.
///
/// # Safety
///
/// Replaces the process's handler for `signal`.
#[cfg(target_os = "linux")]
unsafe fn register(
    signal: libc::c_int,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::io::Result<()> {
    let _ = STOP.set(stop);
    let handler: extern "C" fn(libc::c_int) = on_signal;
    // SAFETY: installs a handler that only stores to an atomic.
    if unsafe { libc::signal(signal, handler as libc::sighandler_t) } == libc::SIG_ERR {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("dsa-devices runs on Linux only");
    std::process::exit(1);
}
