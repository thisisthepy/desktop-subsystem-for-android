//! `dsa-channeld`: the guest's control channel daemon, as init starts it.
//!
//!     dsa-channeld [--port <port>] [--bootconfig <path>] [--cid <cid>]
//!
//! It reads the boot's secret from the kernel's bootconfig, connects to the host over
//! vsock, and keeps the connection, connecting again whenever it is lost. It exits only
//! when it cannot work at all: no secret, or a host that refuses this guest for good.
//! Messages go to standard error, which init sends to the kernel log.

#[cfg(any(target_os = "linux", target_os = "android"))]
fn main() {
    std::process::exit(daemon::run());
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn main() {
    eprintln!("dsa-channeld runs only in the Linux guest");
    std::process::exit(2);
}

/// Writes one message line to standard error in a single write. The kernel log makes a
/// record of every write, so a line written in pieces arrives cut into pieces.
#[cfg(any(target_os = "linux", target_os = "android"))]
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write;
        let line = format!("dsa-channeld: {}\n", format_args!($($arg)*));
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }};
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod daemon {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::thread;

    use dsa_guest_channeld::{ChannelEvent, Credentials, RetryPolicy};
    use dsa_guest_transport::vsock::{VsockConnector, VMADDR_CID_HOST};
    use dsa_protocol::auth::{secret_from_bootconfig, BOOTCONFIG_KEY};
    use dsa_protocol::control::Role;
    use dsa_protocol::handshake::local_hello;

    /// The host's control channel port unless the command line names another. The host's
    /// configuration has the same default.
    const DEFAULT_PORT: u32 = 5000;
    const DEFAULT_BOOTCONFIG: &str = "/proc/bootconfig";

    struct Options {
        port: u32,
        bootconfig: String,
        /// The host's vsock address; another only for a test over vsock loopback.
        cid: u32,
    }

    fn options() -> Result<Options, String> {
        let mut options = Options {
            port: DEFAULT_PORT,
            bootconfig: DEFAULT_BOOTCONFIG.into(),
            cid: VMADDR_CID_HOST,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or(format!("{arg} needs a value"));
            match arg.as_str() {
                "--port" => {
                    let text = value()?;
                    options.port = text
                        .parse()
                        .map_err(|_| format!("--port {text} is not a port number"))?;
                }
                "--bootconfig" => options.bootconfig = value()?,
                "--cid" => {
                    let text = value()?;
                    options.cid = text
                        .parse()
                        .map_err(|_| format!("--cid {text} is not a vsock address"))?;
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(options)
    }

    pub fn run() -> i32 {
        let options = match options() {
            Ok(options) => options,
            Err(why) => {
                say!("{why}");
                return 2;
            }
        };
        let secret = match std::fs::read_to_string(&options.bootconfig) {
            Ok(text) => match secret_from_bootconfig(&text) {
                Some(secret) => secret,
                None => {
                    say!(
                        "{} has no {BOOTCONFIG_KEY}; the host did not start this \
                         boot, so there is nothing to connect to",
                        options.bootconfig
                    );
                    return 1;
                }
            },
            Err(error) => {
                say!("could not read {}: {error}", options.bootconfig);
                return 1;
            }
        };
        let credentials = Credentials {
            hello: local_hello(
                Role::Guest,
                concat!("dsa-channeld ", env!("CARGO_PKG_VERSION")),
            ),
            secret,
        };
        let mut connector = VsockConnector {
            cid: options.cid,
            port: options.port,
        };

        // Nobody in the guest takes the host's frames yet; the agent and the hwcomposer will
        // reach this daemon over binder. Until then they are counted and dropped.
        let (to_guest, from_host) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut dropped = 0u64;
            for _ in from_host {
                dropped += 1;
                if dropped.is_power_of_two() {
                    say!("{dropped} frames from the host had no taker");
                }
            }
        });
        // Kept so the daemon's own end of the channel stays open.
        let (_to_host, from_guest) = mpsc::channel::<Vec<u8>>();
        let (events, event_rx) = mpsc::channel();
        thread::spawn(move || {
            for event in event_rx {
                match event {
                    ChannelEvent::Connected { host_build } => {
                        say!("connected to the host ({host_build})")
                    }
                    ChannelEvent::Lost(why) => {
                        say!("lost the host ({why}); connecting again")
                    }
                }
            }
        });

        say!("connecting to the host on vsock port {}", options.port);
        let stop = AtomicBool::new(false);
        let ended = dsa_guest_channeld::run(
            &mut connector,
            &credentials,
            RetryPolicy::default(),
            &to_guest,
            &from_guest,
            &events,
            &stop,
        );
        say!("{ended}");
        1
    }
}
