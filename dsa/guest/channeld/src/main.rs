//! `dsa-channeld`: the guest's control channel daemon, as init starts it.
//!
//!     dsa-channeld [--port <port>] [--log-port <port>] [--bootconfig <path>] [--cid <cid>]
//!
//! It reads the boot's secret from the kernel's bootconfig, connects to the host over
//! vsock, and keeps the connection, connecting again whenever it is lost. While the host
//! asks for Android's log, it reads logd and sends the records on the log port, a vsock port
//! of its own. It exits only
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
    use std::sync::Arc;
    use std::thread;

    use dsa_guest_channeld::logbridge::{
        files_in, DiagnosticFiles, LogBridge, LogPackets, LogSource, PortConnector,
    };
    use dsa_guest_channeld::logd::{LogdSocket, LOGDR_SOCKET};
    use dsa_guest_channeld::router::{Peer, Router};
    use dsa_guest_channeld::{ChannelEvent, Credentials, RetryPolicy};
    use dsa_guest_transport::vsock::{VsockConnector, VMADDR_CID_HOST};
    use dsa_protocol::auth::{secret_from_bootconfig, BOOTCONFIG_KEY};
    use dsa_protocol::control::Role;
    use dsa_protocol::handshake::{guest_connect, local_hello};
    use dsa_transport::Connector;

    /// The host's control channel port unless the command line names another. The host's
    /// configuration has the same default.
    const DEFAULT_PORT: u32 = 5000;
    /// The host's log port unless the command line names another; the host's configuration
    /// has the same default.
    const DEFAULT_LOG_PORT: u32 = 5001;
    const DEFAULT_BOOTCONFIG: &str = "/proc/bootconfig";

    /// logd's reader socket.
    struct Logd;

    struct LogdPackets(LogdSocket);

    impl LogPackets for LogdPackets {
        fn next(&mut self, timeout: std::time::Duration) -> Result<Option<Vec<u8>>, String> {
            let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
            self.0.next(millis).map_err(|e| e.to_string())
        }
    }

    impl LogSource for Logd {
        fn open(&self, request: &str) -> Result<Box<dyn LogPackets>, String> {
            LogdSocket::open(LOGDR_SOCKET, request)
                .map(|socket| Box::new(LogdPackets(socket)) as Box<dyn LogPackets>)
                .map_err(|e| format!("{LOGDR_SOCKET}: {e}"))
        }
    }

    /// What the kernel kept of the boot before, in pstore: the last kernel log of a guest that
    /// crashed, and Android's last log lines.
    struct Pstore;

    impl DiagnosticFiles for Pstore {
        fn files(&self) -> Vec<(String, Result<Vec<u8>, String>)> {
            files_in(std::path::Path::new("/sys/fs/pstore"), "android/pstore")
        }
    }

    struct Options {
        port: u32,
        log_port: u32,
        bootconfig: String,
        /// The host's vsock address; another only for a test over vsock loopback.
        cid: u32,
    }

    fn options() -> Result<Options, String> {
        let mut options = Options {
            port: DEFAULT_PORT,
            log_port: DEFAULT_LOG_PORT,
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
                "--log-port" => {
                    let text = value()?;
                    options.log_port = text
                        .parse()
                        .map_err(|_| format!("--log-port {text} is not a port number"))?;
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

    /// Registers the binder service the guest's clients call. Without it the daemon still
    /// holds the connection, so the host sees the guest, but no app task gets a window.
    #[cfg(target_os = "android")]
    fn serve_clients(router: &Arc<Router>) {
        match dsa_guest_channeld::binder::serve(Arc::clone(router)) {
            Ok(()) => say!(
                "serving {} to the guest's clients",
                dsa_guest_channeld::aidl::INSTANCE
            ),
            Err(why) => say!("the guest's clients cannot reach the daemon: {why}"),
        }
    }

    /// A Linux system other than Android has no servicemanager to register with.
    #[cfg(target_os = "android")]
    fn check_agent(peer: Peer) -> Result<(), String> {
        dsa_guest_channeld::binder::check_agent(peer)
    }

    /// A Linux system other than Android has no agent.
    #[cfg(not(target_os = "android"))]
    fn check_agent(_peer: Peer) -> Result<(), String> {
        Err("this system has no agent".to_string())
    }

    #[cfg(not(target_os = "android"))]
    fn serve_clients(_router: &Arc<Router>) {
        say!("no binder service off Android; only the connection runs");
    }

    /// Sets a system property through bionic, which hands it to init.
    #[cfg(target_os = "android")]
    fn set_property(name: &str, value: &str) -> Result<(), String> {
        use std::ffi::{c_char, c_int, CString};
        extern "C" {
            fn __system_property_set(name: *const c_char, value: *const c_char) -> c_int;
        }
        let (c_name, c_value) = (
            CString::new(name).map_err(|e| e.to_string())?,
            CString::new(value).map_err(|e| e.to_string())?,
        );
        // SAFETY: both are valid NUL-terminated strings that outlive the call.
        match unsafe { __system_property_set(c_name.as_ptr(), c_value.as_ptr()) } {
            0 => Ok(()),
            code => Err(format!("init refused {name}={value} ({code})")),
        }
    }

    /// Tells the router once Android has finished booting. Init sets the property the daemon
    /// may read when `sys.boot_completed` is set (`dsa-channeld.rc`). The property lasts for
    /// the boot, and the daemon restarts within it, so a late look still sees it.
    #[cfg(target_os = "android")]
    fn watch_boot(router: Arc<Router>) {
        use std::ffi::{c_char, c_int, CStr};
        extern "C" {
            fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
        }
        /// bionic's `PROP_VALUE_MAX`.
        const VALUE_MAX: usize = 92;
        let name = c"vendor.dsa.boot_completed";
        loop {
            let mut value = [0 as c_char; VALUE_MAX];
            // SAFETY: a NUL-terminated name and a buffer of the size bionic writes at most.
            unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
            // SAFETY: bionic leaves a NUL-terminated string there, empty if the property is unset.
            if unsafe { CStr::from_ptr(value.as_ptr()) }.to_bytes() == b"1" {
                say!("Android has finished booting");
                router.boot_completed();
                return;
            }
            thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// A Linux system other than Android has no such property.
    #[cfg(not(target_os = "android"))]
    fn watch_boot(_router: Arc<Router>) {}

    /// Has init start adbd (developer mode on) or stop it (`dsa-channeld.rc`). Only init may
    /// start a service; the daemon sets the property it watches.
    #[cfg(target_os = "android")]
    fn switch_adb(on: bool) {
        let value = if on { "1" } else { "0" };
        match set_property("vendor.dsa.adb", value) {
            Ok(()) => say!(
                "developer mode is {}",
                if on { "on: adbd starts" } else { "off" }
            ),
            Err(why) => say!("could not switch adbd for developer mode: {why}"),
        }
    }

    /// Off Android there is no adbd to switch.
    #[cfg(not(target_os = "android"))]
    fn switch_adb(_on: bool) {}

    /// Hands idle memory back to the host: every so often, decides whether the guest should
    /// reclaim now (see `reclaim`), and has init do it. Only init may write the
    /// kernel's memory controls; the daemon only asks.
    #[cfg(target_os = "android")]
    fn reclaim_idle_memory(router: Arc<Router>) {
        use dsa_guest_channeld::reclaim::{
            memory_pressure, MemInfo, Reason, ReclaimPolicy, Reclaimer, CHECK_INTERVAL,
            RECLAIM_PROPERTY, TRIM_PROPERTY,
        };
        let start = std::time::Instant::now();
        let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
        let mut closed = router.apps_closed();
        loop {
            let now_closed = router.wait_for_closed_app(closed, CHECK_INTERVAL);
            if now_closed != closed {
                closed = now_closed;
                // An app's last window closed: the agent ends its processes, and what they
                // held and the cache they read go back to the host now, not at the next look.
                // The pause lets the processes die first.
                thread::sleep(std::time::Duration::from_secs(2));
                say!("an app closed: dropping its caches so its memory goes back to the host");
                if let Err(why) = set_property(RECLAIM_PROPERTY, "1") {
                    say!("could not ask init to reclaim: {why}");
                }
                continue;
            }
            let memory = match std::fs::read_to_string("/proc/meminfo") {
                Ok(text) => MemInfo::parse(&text),
                Err(error) => {
                    say!("cannot read /proc/meminfo, so idle memory is not reclaimed: {error}");
                    return;
                }
            };
            let tasks = router.tasks_in_use();
            reclaimer.host_short(router.host_memory_low());
            // Pressure stall information is optional in the kernel: unreadable counts as busy.
            let pressure = std::fs::read_to_string("/proc/pressure/memory")
                .map(|text| memory_pressure(&text, 5.0))
                .unwrap_or(true);
            if let Some(reason) = reclaimer.decide_under(start.elapsed(), tasks, &memory, pressure)
            {
                let property = if reason == Reason::Gradual {
                    TRIM_PROPERTY
                } else {
                    RECLAIM_PROPERTY
                };
                say!(
                    "reclaiming the guest's memory ({reason:?}, {tasks} tasks in use): {} MiB \
                     droppable and {} MiB anonymous of {} MiB in use",
                    memory.droppable_kb() / 1024,
                    memory.anon_pages_kb / 1024,
                    memory.used_kb() / 1024
                );
                if let Err(why) = set_property(property, "1") {
                    say!("could not ask init to reclaim: {why}");
                }
            }
        }
    }

    /// Off Android there is no init to ask, and the host is not Hyper-V.
    #[cfg(not(target_os = "android"))]
    fn reclaim_idle_memory(_router: Arc<Router>) {}

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

        // The guest's clients (the agent and the hwcomposer) reach the daemon over binder.
        // Frames they send go to the host through `to_host`; the host's frames go to them.
        let (to_host, from_guest) = mpsc::channel::<Vec<u8>>();
        let router = Arc::new(Router::new(to_host, check_agent).on_developer_mode(switch_adb));
        let (to_guest, from_host) = mpsc::channel::<Vec<u8>>();
        // The guest's log goes on a port of its own, past the same handshake as the control
        // channel, and only while the host asks for it.
        let connect_log: Arc<PortConnector> = {
            let (credentials, cid, port) = (credentials.clone(), options.cid, options.log_port);
            Arc::new(move || {
                let mut connection = VsockConnector { cid, port }
                    .connect(std::time::Duration::from_secs(10))
                    .map_err(|e| e.to_string())?;
                guest_connect(
                    connection.as_mut(),
                    &credentials.hello,
                    &credentials.secret,
                    std::time::Duration::from_secs(10),
                )
                .map_err(|e| e.to_string())?;
                Ok(connection)
            })
        };
        let bridge = {
            let router = Arc::clone(&router);
            Arc::new(LogBridge::new(
                Arc::new(Logd),
                Arc::new(Pstore),
                connect_log,
                Arc::new(move || router.agent_connected()),
            ))
        };
        router.divert_diagnostics(bridge.agent_frames());
        {
            let (router, bridge) = (Arc::clone(&router), Arc::clone(&bridge));
            thread::spawn(move || {
                for frame in from_host {
                    if !bridge.from_host(&frame) {
                        router.from_host(&frame);
                    }
                }
            });
        }
        serve_clients(&router);
        {
            let router = Arc::clone(&router);
            thread::spawn(move || reclaim_idle_memory(router));
        }
        {
            let router = Arc::clone(&router);
            thread::spawn(move || watch_boot(router));
        }
        let (events, event_rx) = mpsc::channel();
        thread::spawn(move || {
            for event in event_rx {
                match event {
                    ChannelEvent::Connected { host_build } => {
                        say!("connected to the host ({host_build})");
                        // A host that connects again, or a new host, learns the tasks the
                        // guest already has; the clients report a task only when it changes.
                        router.host_connected();
                    }
                    ChannelEvent::Lost(why) => {
                        // Nobody reads the log until a host asks again.
                        bridge.host_lost();
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
