//! `dsa-devices` on macOS: lends the Mac's devices to the user's Android, from inside the
//! user's login session.
//!
//! ```text
//! dsa-devices [--socket <path>] [--no-location] [--no-print] [--no-links]
//!             [--no-settings] [--no-gamepads] [--no-passkeys] [--no-language] [--no-geocoding]
//!             [--no-share] [--no-device-ports]
//! dsa-devices version
//! ```
//!
//! Sound, video decoding, the cameras, the sensors, the location, printing, web links, the
//! game controllers and the Mac's settings all need the user's session: its sound devices,
//! its privacy prompts, its print panel, its browser. So this program runs as the user,
//! started at login by its launchd agent (installed by `dsa-hostd install-agent`). It serves
//! the guest's device ports, which `dsa-hostd` relays from the VM to sockets in the user's
//! DSA folder (`vsock/<port>.sock`), and follows the local API at the user's socket (or
//! `--socket <path>`).
//!
//! The passkey prompts of the user's Android apps come here too: the Mac's passkeys are used
//! through AuthenticationServices, whose prompt needs a window of the user's session.
//!
//! The main thread runs AppKit, as an app without a Dock icon, so CoreLocation, the game
//! controllers, the print panel and the passkey prompt have their run loop. It runs until it is stopped (SIGTERM,
//! which launchd sends at logout, or SIGINT), and logs to standard error, which its agent
//! sends to `dsa-devices.log` in the user's DSA logs.
//!
//! macOS reads the reasons it shows when it asks for the camera, the microphone and the
//! location from the Info.plist embedded in this program.

/// The program's Info.plist, where macOS finds its identity and the reasons for its prompts.
#[cfg(target_os = "macos")]
#[used]
#[link_section = "__TEXT,__info_plist"]
static INFO_PLIST: [u8; include_bytes!("../../packaging/dsa-devices-Info.plist").len()] =
    *include_bytes!("../../packaging/dsa-devices-Info.plist");

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-devices: this build is the macOS device lender and runs on macOS only");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use dsa_host_macos::devices::{self, ConnectorFactory, Followers, Log};
    use dsa_host_macos::host_config::{user_root_in, MacHostConfig, UserFolders};
    use dsa_host_unix::socket::UnixLocalConnector;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    const HELP: &str = "usage: dsa-devices [--socket <path>] [--no-location] [--no-print] \
                        [--no-links] [--no-settings] [--no-gamepads] [--no-passkeys] [--no-language] \
                        [--no-geocoding] [--no-share] [--no-device-ports] \
                        | version";

    static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    extern "C" fn on_signal(_: libc::c_int) {
        if let Some(stop) = STOP.get() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    let mut socket: Option<PathBuf> = None;
    let mut which = Followers::default();
    let mut ports = true;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => match args.next() {
                Some(path) => socket = Some(PathBuf::from(path)),
                None => {
                    eprintln!("dsa-devices: --socket needs a path\n{HELP}");
                    return std::process::ExitCode::from(2);
                }
            },
            "--no-location" => which.location = false,
            "--no-print" => which.print = false,
            "--no-passkeys" => which.passkeys = false,
            "--no-links" => which.links = false,
            "--no-settings" => which.settings = false,
            "--no-gamepads" => which.gamepads = false,
            "--no-language" => which.language = false,
            "--no-geocoding" => which.geocoding = false,
            "--no-share" => which.share = false,
            "--no-device-ports" => ports = false,
            "version" | "--version" => {
                println!("dsa-devices {}", dsa_host_macos::VERSION);
                return std::process::ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                println!("{HELP}");
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("dsa-devices: unknown argument {other:?}\n{HELP}");
                return std::process::ExitCode::from(2);
            }
        }
    }

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("dsa-devices: HOME is not set, so DSA's folder cannot be found");
        return std::process::ExitCode::FAILURE;
    };
    // The same folders the agent uses: its configuration's, else the defaults.
    let config = MacHostConfig::find(&home).and_then(|path| match MacHostConfig::read(&path) {
        Ok(config) => Some(config),
        Err(why) => {
            eprintln!("dsa-devices: {why}; using the default folders");
            None
        }
    });
    let user = match &config {
        Some(config) => config.user(&home),
        None => UserFolders {
            root: user_root_in(&home),
        },
    };
    let socket = socket.unwrap_or_else(|| match &config {
        Some(config) => config.socket(&user),
        None => user.socket(),
    });

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
        // A guest that goes away mid-write must not end the lender.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    // AppKit first, on the main thread: an app with no Dock icon and no menu bar, whose
    // print panel may still come to the front.
    let app = unsafe {
        let Ok(app_class) = dsa_host_macos::objc::class(c"NSApplication") else {
            eprintln!("dsa-devices: this macOS has no NSApplication");
            return std::process::ExitCode::FAILURE;
        };
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        // NSApplicationActivationPolicyAccessory.
        let _: bool = msg_send![app, setActivationPolicy: 1isize];
        app
    };

    let log: Log = Arc::new(|line: &str| eprintln!("{line}"));
    let connect: ConnectorFactory = Arc::new(move || {
        Box::new(UnixLocalConnector {
            path: socket.clone(),
        }) as Box<dyn dsa_transport::Connector>
    });
    let mut threads = Vec::new();
    if ports {
        threads.extend(devices::start_guest_ports(&connect, &user, &stop, &log));
    }
    threads.extend(devices::start_followers(connect, which, &stop, &log));
    eprintln!(
        "dsa-devices {}: running for {}",
        dsa_host_macos::VERSION,
        user.root.display()
    );

    // When stopped: let every device end (each looks at the flag within a second), then end
    // the run loop, and with it the program.
    {
        let stop = Arc::clone(&stop);
        let spawned = std::thread::Builder::new()
            .name("dsa-devices-stop".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                eprintln!("dsa-devices: stopping");
                // A device stuck in a prompt or a print panel is not waited for long.
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                for thread in threads {
                    while !thread.is_finished() && std::time::Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    if thread.is_finished() {
                        let _ = thread.join();
                    }
                }
                dsa_host_macos::objc::on_main(|| {
                    std::process::exit(0);
                });
            });
        if let Err(error) = spawned {
            eprintln!("dsa-devices: cannot watch for the stop signal: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }

    // SAFETY: the shared application, on the main thread.
    unsafe {
        let _: () = msg_send![app, run];
    }
    std::process::ExitCode::SUCCESS
}
