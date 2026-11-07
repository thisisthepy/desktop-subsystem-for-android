//! How adb on this computer finds developer mode's port without `adb connect`: the host
//! attaches it to the user's adb server, and advertises it over mDNS.
//!
//! **Auto-connect.** While developer mode and the auto-connect setting are both on, the host
//! watches the user's adb server (`127.0.0.1:5037`, or the port `ANDROID_ADB_SERVER_PORT`
//! names). When the server runs and does not list `127.0.0.1:<adb port>`, the host asks it to
//! connect, with the server's own `host:connect` request, so Android Studio and `adb devices`
//! show DSA with nothing typed. adb itself is never looked for or run: the host speaks the adb
//! server protocol on the server's socket. A request is four hex digits giving its length,
//! then the request; the server answers `OKAY` followed by a length-prefixed answer, or `FAIL`
//! followed by a length-prefixed reason.
//!
//! The host keeps one `host:track-devices` connection open to the server while it watches.
//! The server holds it until the server ends, so its end marks the end of that server
//! instance: a restart is seen even when the new server is up again before the next look.
//!
//! A user who runs `adb disconnect` means it. When DSA was attached as a ready device, the
//! same server instance still runs, and DSA is gone from its list, the host leaves it alone
//! until that server ends and another starts. Turning developer mode off detaches DSA with
//! `host:disconnect`. Turning auto-connect off stops the watching and sends nothing at all.
//!
//! **mDNS.** While developer mode is on, the platform advertises the port as an `_adb._tcp`
//! service whose only address is `127.0.0.1`, so a tool on this computer that browses for adb
//! devices finds it and no other device is given an address it could reach.
//!
//! Nothing here names a host operating system API. The platform hands in how to reach the
//! user's adb server ([`AdbServerLink`], usually [`loopback_link`] with the user's server
//! port) and how to advertise ([`AdbAdvertise`]).

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// The port the adb server listens on when `ANDROID_ADB_SERVER_PORT` does not name another.
pub const DEFAULT_ADB_SERVER_PORT: u16 = 5037;

/// The environment variable adb reads for its server's port.
pub const ADB_SERVER_PORT_VARIABLE: &str = "ANDROID_ADB_SERVER_PORT";

/// How often the host looks at the adb server. A look is one or two short loopback requests.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The mDNS service type adb browses for.
pub const MDNS_SERVICE_TYPE: &str = "_adb._tcp";

/// How long connecting to the adb server may take. It is on the loopback address, so a
/// server that takes longer is not answering.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// How long one answer of the adb server may take. `host:connect` waits for the TCP connect
/// to DSA's own loopback port, which is quick.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// The most connect attempts in a row that wait an extra look each before the next one.
const MAX_BACKOFF_LOOKS: u32 = 15;

/// The port of the adb server, from the value of `ANDROID_ADB_SERVER_PORT` if the user set
/// one. A value that is not a port from 1 to 65535 is ignored, as adb ignores it.
pub fn adb_server_port(variable: Option<&str>) -> u16 {
    variable
        .and_then(|value| value.trim().parse::<u16>().ok())
        .filter(|port| *port != 0)
        .unwrap_or(DEFAULT_ADB_SERVER_PORT)
}

/// The serial adb gives developer mode's port once it is connected.
pub fn serial(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// The mDNS instance name of developer mode's port. adb shows it in `adb mdns services`.
pub fn mdns_instance_name(port: u16) -> String {
    format!("adb-DSA-{port}")
}

/// One connection to the adb server.
pub trait AdbServerStream: Read + Write + Send {
    /// Makes reads return at once with [`io::ErrorKind::WouldBlock`] when nothing has come,
    /// so the host can see whether the server is still there without waiting on it.
    fn set_nonblocking(&self, on: bool) -> io::Result<()>;
}

impl AdbServerStream for TcpStream {
    fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        TcpStream::set_nonblocking(self, on)
    }
}

/// Opens a connection to the user's adb server. It fails when no server listens.
pub type AdbServerLink = Arc<dyn Fn() -> io::Result<Box<dyn AdbServerStream>> + Send + Sync>;

/// Advertises developer mode's port over mDNS as an `_adb._tcp` service on `127.0.0.1` only.
/// The advertisement lasts until the value it returns is dropped. `None` when the platform
/// cannot advertise; it says why in its log.
pub type AdbAdvertise = Arc<dyn Fn(u16) -> Option<Box<dyn Send>> + Send + Sync>;

/// A link to the adb server on `127.0.0.1` at the port `port` gives each time it connects, so a
/// user who changes `ANDROID_ADB_SERVER_PORT` is followed.
pub fn loopback_link(port: impl Fn() -> u16 + Send + Sync + 'static) -> AdbServerLink {
    Arc::new(move || {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port()));
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT))?;
        let _ = stream.set_nodelay(true);
        Ok(Box::new(stream) as Box<dyn AdbServerStream>)
    })
}

/// What went wrong with a request to the adb server.
#[derive(Debug)]
pub enum AdbError {
    /// The server could not be reached, or the connection broke.
    Io(io::Error),
    /// The server refused the request, and said why.
    Fail(String),
    /// The server answered with something that is not the adb server protocol.
    Malformed(String),
}

impl std::fmt::Display for AdbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdbError::Io(error) => write!(f, "{error}"),
            AdbError::Fail(why) => write!(f, "the adb server refused: {why}"),
            AdbError::Malformed(what) => write!(f, "the adb server's answer is unreadable: {what}"),
        }
    }
}

impl From<io::Error> for AdbError {
    fn from(error: io::Error) -> AdbError {
        AdbError::Io(error)
    }
}

/// Writes one request: its length in four hex digits, then the request.
pub fn send_request(stream: &mut dyn Write, request: &str) -> Result<(), AdbError> {
    if request.len() > 0xffff {
        return Err(AdbError::Malformed(format!(
            "a request of {} bytes is longer than adb takes",
            request.len()
        )));
    }
    stream.write_all(format!("{:04x}{request}", request.len()).as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Reads the server's `OKAY`, or its `FAIL` and the reason that follows.
pub fn read_status(stream: &mut dyn Read) -> Result<(), AdbError> {
    let mut status = [0u8; 4];
    stream.read_exact(&mut status)?;
    match &status {
        b"OKAY" => Ok(()),
        b"FAIL" => Err(AdbError::Fail(read_block(stream)?)),
        other => Err(AdbError::Malformed(format!(
            "{:?} where OKAY or FAIL belongs",
            String::from_utf8_lossy(other)
        ))),
    }
}

/// Reads one length-prefixed block: four hex digits, then that many bytes.
pub fn read_block(stream: &mut dyn Read) -> Result<String, AdbError> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = std::str::from_utf8(&length)
        .ok()
        .and_then(|digits| usize::from_str_radix(digits, 16).ok())
        .ok_or_else(|| {
            AdbError::Malformed(format!(
                "{:?} is not a length",
                String::from_utf8_lossy(&length)
            ))
        })?;
    let mut block = vec![0u8; length];
    stream.read_exact(&mut block)?;
    Ok(String::from_utf8_lossy(&block).into_owned())
}

/// Sends one `host:` request on a new connection and returns the server's answer.
pub fn query(link: &AdbServerLink, request: &str) -> Result<String, AdbError> {
    let mut stream = link()?;
    send_request(&mut stream, request)?;
    read_status(&mut stream)?;
    read_block(&mut stream)
}

/// One device in the server's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    /// `device` once it answers, `offline`, `connecting`, `unauthorized` and so on.
    pub state: String,
}

/// The devices of a `host:devices` answer: one `serial<TAB>state` line each.
pub fn parse_devices(list: &str) -> Vec<AdbDevice> {
    list.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let serial = fields.next()?.trim();
            let state = fields.next().unwrap_or("").trim();
            (!serial.is_empty()).then(|| AdbDevice {
                serial: serial.to_string(),
                state: state.to_string(),
            })
        })
        .collect()
}

/// Whether a `host:connect` answer says the device is connected.
fn connected(answer: &str) -> bool {
    let answer = answer.trim_start();
    answer.starts_with("connected to") || answer.starts_with("already connected to")
}

/// Where auto-connect stands, as the settings apps show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoConnectState {
    /// Developer mode or auto-connect is off, so the host does not watch the adb server.
    #[default]
    Off,
    /// No adb server runs on this computer yet. The host attaches DSA when one starts.
    WaitingForServer,
    /// The adb server runs, but Android is not running or adb in Android has not answered yet.
    WaitingForAndroid,
    /// The adb server lists DSA as a ready device.
    Attached,
    /// The user detached DSA from this adb server. The host attaches it again only once the
    /// server restarts.
    Detached,
}

impl AutoConnectState {
    /// The name the local API and the CLI use.
    pub fn as_str(self) -> &'static str {
        match self {
            AutoConnectState::Off => "off",
            AutoConnectState::WaitingForServer => "waiting-for-server",
            AutoConnectState::WaitingForAndroid => "waiting-for-android",
            AutoConnectState::Attached => "attached",
            AutoConnectState::Detached => "detached",
        }
    }

    /// The state a name stands for. An empty or unknown name, as an older service sends, is
    /// [`AutoConnectState::Off`].
    pub fn parse(name: &str) -> AutoConnectState {
        match name {
            "waiting-for-server" => AutoConnectState::WaitingForServer,
            "waiting-for-android" => AutoConnectState::WaitingForAndroid,
            "attached" => AutoConnectState::Attached,
            "detached" => AutoConnectState::Detached,
            _ => AutoConnectState::Off,
        }
    }
}

impl std::fmt::Display for AutoConnectState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What auto-connect is asked to do at one look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wanted {
    /// Developer mode's loopback port while developer mode is on.
    pub developer_mode_port: Option<u16>,
    /// Whether the user's auto-connect setting is on.
    pub auto_connect: bool,
    /// Whether the user's Android runs and can answer adb.
    pub guest_ready: bool,
}

/// Auto-connect's decisions, one look at a time. [`AutoConnectWatcher`] runs it on a thread;
/// tests call [`AutoConnect::look`] themselves.
pub struct AutoConnect {
    link: AdbServerLink,
    /// The `host:track-devices` connection of the server instance being watched.
    instance: Option<Box<dyn AdbServerStream>>,
    /// The port DSA asked this server to connect, or saw it list.
    ours: Option<u16>,
    /// The port this server listed as a ready device.
    attached: Option<u16>,
    /// Set when the user detached DSA from this server instance.
    stay_away: bool,
    /// Connect attempts in a row that did not end with DSA ready, and the looks still to wait
    /// before the next.
    attempts: u32,
    wait_looks: u32,
    state: AutoConnectState,
}

impl AutoConnect {
    pub fn new(link: AdbServerLink) -> AutoConnect {
        AutoConnect {
            link,
            instance: None,
            ours: None,
            attached: None,
            stay_away: false,
            attempts: 0,
            wait_looks: 0,
            state: AutoConnectState::Off,
        }
    }

    /// The state after the last look.
    pub fn state(&self) -> AutoConnectState {
        self.state
    }

    /// Looks at the adb server once and does what `wanted` calls for.
    pub fn look(&mut self, wanted: Wanted) -> AutoConnectState {
        let state = self.decide(wanted);
        if state != self.state {
            log_change(state, wanted.developer_mode_port);
        }
        self.state = state;
        state
    }

    fn decide(&mut self, wanted: Wanted) -> AutoConnectState {
        if self.instance.is_some() && !self.instance_alive() {
            eprintln!("adb auto-connect: the adb server stopped");
            self.forget_instance();
        }
        if !wanted.auto_connect {
            // Off means hands off: nothing more is sent, not even a disconnect.
            self.forget_instance();
            return AutoConnectState::Off;
        }
        let Some(port) = wanted.developer_mode_port else {
            if let Some(old) = self.ours.take() {
                if self.instance.is_some() {
                    self.disconnect(old);
                }
            }
            self.forget_instance();
            return AutoConnectState::Off;
        };
        if let Some(old) = self.ours.filter(|old| *old != port) {
            // The user chose another port: the old one is closing.
            self.disconnect(old);
            self.ours = None;
            self.attached = None;
            self.reset_attempts();
        }
        if self.instance.is_none() {
            match self.track() {
                Ok(tracker) => {
                    self.instance = Some(tracker);
                    self.stay_away = false;
                    self.reset_attempts();
                }
                Err(_) => return AutoConnectState::WaitingForServer,
            }
        }
        let devices = match query(&self.link, "host:devices") {
            Ok(list) => parse_devices(&list),
            Err(AdbError::Io(_)) => {
                self.forget_instance();
                return AutoConnectState::WaitingForServer;
            }
            Err(why) => {
                eprintln!("adb auto-connect: listing the adb server's devices: {why}");
                return AutoConnectState::WaitingForServer;
            }
        };
        if !wanted.guest_ready {
            // Android is stopped, starting or frozen: its going from the list is not the
            // user's doing, and connecting would reach nothing.
            self.attached = None;
            return AutoConnectState::WaitingForAndroid;
        }
        let serial = serial(port);
        match devices.iter().find(|device| device.serial == serial) {
            Some(device) if device.state == "device" => {
                self.ours = Some(port);
                self.attached = Some(port);
                self.reset_attempts();
                AutoConnectState::Attached
            }
            // Listed but not answering yet: the server is still connecting.
            Some(_) => {
                self.ours = Some(port);
                AutoConnectState::WaitingForAndroid
            }
            None if self.attached == Some(port) => {
                // It was ready, the same server still runs, and it is gone: the user
                // detached it.
                eprintln!(
                    "adb auto-connect: {serial} was detached from the adb server (adb \
                     disconnect); it is attached again when the adb server restarts"
                );
                self.stay_away = true;
                self.attached = None;
                self.ours = None;
                AutoConnectState::Detached
            }
            None if self.stay_away => AutoConnectState::Detached,
            None => {
                if self.wait_looks > 0 {
                    self.wait_looks -= 1;
                    return AutoConnectState::WaitingForAndroid;
                }
                self.attempts += 1;
                self.wait_looks = (1u32 << self.attempts.min(4)).min(MAX_BACKOFF_LOOKS + 1) - 1;
                match query(&self.link, &format!("host:connect:{serial}")) {
                    Ok(answer) if connected(&answer) => {
                        self.ours = Some(port);
                        AutoConnectState::WaitingForAndroid
                    }
                    Ok(answer) => {
                        eprintln!(
                            "adb auto-connect: the adb server did not connect {serial}: {answer}"
                        );
                        AutoConnectState::WaitingForAndroid
                    }
                    Err(AdbError::Io(_)) => {
                        self.forget_instance();
                        AutoConnectState::WaitingForServer
                    }
                    Err(why) => {
                        eprintln!("adb auto-connect: connecting {serial}: {why}");
                        AutoConnectState::WaitingForAndroid
                    }
                }
            }
        }
    }

    /// Opens the connection that lasts as long as the server instance.
    fn track(&self) -> Result<Box<dyn AdbServerStream>, AdbError> {
        let mut stream = (self.link)()?;
        send_request(&mut stream, "host:track-devices")?;
        read_status(&mut stream)?;
        stream.set_nonblocking(true)?;
        Ok(stream)
    }

    /// Whether the server instance's tracking connection is still open. What the server sent
    /// on it (each change of its list) is read and dropped.
    fn instance_alive(&mut self) -> bool {
        let Some(tracker) = self.instance.as_mut() else {
            return false;
        };
        let mut buffer = [0u8; 1024];
        loop {
            match tracker.read(&mut buffer) {
                Ok(0) => return false,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
    }

    fn disconnect(&self, port: u16) {
        let serial = serial(port);
        // A refusal means it was not connected any longer, and an error that the server is
        // gone: either way there is nothing left to detach.
        if query(&self.link, &format!("host:disconnect:{serial}")).is_ok() {
            eprintln!("adb auto-connect: detached {serial} from the adb server");
        }
    }

    /// Forgets the server instance and all that was learned about it.
    fn forget_instance(&mut self) {
        self.instance = None;
        self.ours = None;
        self.attached = None;
        self.stay_away = false;
        self.reset_attempts();
    }

    fn reset_attempts(&mut self) {
        self.attempts = 0;
        self.wait_looks = 0;
    }
}

fn log_change(state: AutoConnectState, port: Option<u16>) {
    let serial = port.map(serial).unwrap_or_default();
    match state {
        AutoConnectState::Off => eprintln!("adb auto-connect: off"),
        AutoConnectState::WaitingForServer => {
            eprintln!("adb auto-connect: waiting for an adb server on this computer")
        }
        AutoConnectState::WaitingForAndroid => {
            eprintln!("adb auto-connect: waiting for adb in Android to answer at {serial}")
        }
        AutoConnectState::Attached => {
            eprintln!("adb auto-connect: the adb server lists {serial}")
        }
        AutoConnectState::Detached => {}
    }
}

struct Inputs {
    developer_mode_port: Option<u16>,
    auto_connect: bool,
    stop: bool,
    changed: bool,
}

struct WatchShared {
    inputs: Mutex<Inputs>,
    wake: Condvar,
    state: Mutex<AutoConnectState>,
}

/// Runs [`AutoConnect`] on a thread of its own, looking every interval and at once when
/// developer mode or the setting changes. Dropping it detaches DSA as developer mode's end
/// does, then stops.
pub struct AutoConnectWatcher {
    shared: Arc<WatchShared>,
    thread: Option<JoinHandle<()>>,
}

impl AutoConnectWatcher {
    /// Starts watching through `link`. `guest_ready` says whether the user's Android can answer
    /// adb. It starts with developer mode off and auto-connect on, so nothing is sent until
    /// [`AutoConnectWatcher::set_developer_mode`] gives a port.
    pub fn start(
        link: AdbServerLink,
        guest_ready: Arc<dyn Fn() -> bool + Send + Sync>,
        interval: Duration,
    ) -> io::Result<AutoConnectWatcher> {
        let shared = Arc::new(WatchShared {
            inputs: Mutex::new(Inputs {
                developer_mode_port: None,
                auto_connect: true,
                stop: false,
                changed: false,
            }),
            wake: Condvar::new(),
            state: Mutex::new(AutoConnectState::Off),
        });
        let thread = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("dsa-adb-auto-connect".into())
                .spawn(move || watch(AutoConnect::new(link), guest_ready, interval, shared))?
        };
        Ok(AutoConnectWatcher {
            shared,
            thread: Some(thread),
        })
    }

    /// Developer mode's port while it is on, `None` when it is off.
    pub fn set_developer_mode(&self, port: Option<u16>) {
        self.change(|inputs| inputs.developer_mode_port = port);
    }

    /// The user's auto-connect setting.
    pub fn set_auto_connect(&self, on: bool) {
        self.change(|inputs| inputs.auto_connect = on);
    }

    /// Where auto-connect stands after its last look.
    pub fn state(&self) -> AutoConnectState {
        *self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn change(&self, apply: impl FnOnce(&mut Inputs)) {
        let mut inputs = self.shared.inputs.lock().unwrap_or_else(|e| e.into_inner());
        apply(&mut inputs);
        inputs.changed = true;
        self.shared.wake.notify_all();
    }
}

impl Drop for AutoConnectWatcher {
    fn drop(&mut self) {
        self.change(|inputs| inputs.stop = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(
    mut auto: AutoConnect,
    guest_ready: Arc<dyn Fn() -> bool + Send + Sync>,
    interval: Duration,
    shared: Arc<WatchShared>,
) {
    loop {
        let (wanted, stop) = {
            let mut inputs = shared.inputs.lock().unwrap_or_else(|e| e.into_inner());
            inputs.changed = false;
            (
                Wanted {
                    developer_mode_port: inputs.developer_mode_port,
                    auto_connect: inputs.auto_connect,
                    guest_ready: false,
                },
                inputs.stop,
            )
        };
        let wanted = if stop {
            // The end of the service is the end of developer mode's port.
            Wanted {
                developer_mode_port: None,
                ..wanted
            }
        } else {
            Wanted {
                guest_ready: wanted.auto_connect
                    && wanted.developer_mode_port.is_some()
                    && guest_ready(),
                ..wanted
            }
        };
        let state = auto.look(wanted);
        *shared.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        if stop {
            return;
        }
        let inputs = shared.inputs.lock().unwrap_or_else(|e| e.into_inner());
        if !inputs.changed && !inputs.stop {
            let _ = shared
                .wake
                .wait_timeout_while(inputs, interval, |inputs| !inputs.changed && !inputs.stop);
        }
    }
}

/// An adb server for tests that speaks the server protocol on the loopback address, and can
/// stop, restart and forget devices as a real one does.
#[cfg(any(test, feature = "conformance"))]
pub mod fake {
    use super::*;
    use std::net::{Shutdown, TcpListener};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct State {
        /// Where the running instance listens; `None` while the server is down.
        address: Option<SocketAddr>,
        stop: Option<Arc<AtomicBool>>,
        /// The open connections of the running instance, closed when it ends.
        open: Vec<TcpStream>,
        devices: Vec<AdbDevice>,
        requests: Vec<String>,
        connections: usize,
        /// The state a device connected with `host:connect` is listed in.
        connected_state: String,
    }

    /// A fake adb server. It starts running.
    #[derive(Clone)]
    pub struct FakeAdbServer {
        state: Arc<Mutex<State>>,
    }

    impl Default for FakeAdbServer {
        fn default() -> Self {
            FakeAdbServer::start()
        }
    }

    impl FakeAdbServer {
        pub fn start() -> FakeAdbServer {
            let server = FakeAdbServer {
                state: Arc::new(Mutex::new(State {
                    connected_state: "device".into(),
                    ..State::default()
                })),
            };
            server.up();
            server
        }

        /// A link to whichever instance runs now. It is refused while the server is down.
        pub fn link(&self) -> AdbServerLink {
            let state = Arc::clone(&self.state);
            Arc::new(move || {
                let address = state.lock().unwrap().address.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::ConnectionRefused, "no adb server runs")
                })?;
                let stream = TcpStream::connect(address)?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                Ok(Box::new(stream) as Box<dyn AdbServerStream>)
            })
        }

        /// Ends the running instance, as `adb kill-server` does: its port closes, every
        /// connection to it ends, and its device list is gone.
        pub fn stop(&self) {
            let mut state = self.state.lock().unwrap();
            if let Some(stop) = state.stop.take() {
                stop.store(true, Ordering::SeqCst);
            }
            if let Some(address) = state.address.take() {
                // Wakes the accepting thread to see the stop.
                let _ = TcpStream::connect(address);
            }
            for stream in state.open.drain(..) {
                let _ = stream.shutdown(Shutdown::Both);
            }
            state.devices.clear();
        }

        /// Ends the running instance and starts a new one.
        pub fn restart(&self) {
            self.stop();
            self.up();
        }

        /// Removes a device from the list, as `adb disconnect <serial>` does.
        pub fn forget(&self, serial: &str) {
            self.state
                .lock()
                .unwrap()
                .devices
                .retain(|device| device.serial != serial);
        }

        /// Lists `serial`, as a user's own `adb connect` does.
        pub fn add(&self, serial: &str, device_state: &str) {
            let mut state = self.state.lock().unwrap();
            state.devices.retain(|device| device.serial != serial);
            state.devices.push(AdbDevice {
                serial: serial.into(),
                state: device_state.into(),
            });
        }

        /// The state the next devices `host:connect` adds are listed in.
        pub fn connect_as(&self, device_state: &str) {
            self.state.lock().unwrap().connected_state = device_state.into();
        }

        /// Every request the server was sent, in order.
        pub fn requests(&self) -> Vec<String> {
            self.state.lock().unwrap().requests.clone()
        }

        /// The requests that start with `prefix`.
        pub fn requests_starting(&self, prefix: &str) -> Vec<String> {
            self.requests()
                .into_iter()
                .filter(|request| request.starts_with(prefix))
                .collect()
        }

        /// How many connections were made to the server, over every instance.
        pub fn connections(&self) -> usize {
            self.state.lock().unwrap().connections
        }

        /// The devices listed now.
        pub fn devices(&self) -> Vec<AdbDevice> {
            self.state.lock().unwrap().devices.clone()
        }

        fn up(&self) {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            {
                let mut state = self.state.lock().unwrap();
                state.address = Some(address);
                state.stop = Some(Arc::clone(&stop));
            }
            let state = Arc::clone(&self.state);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    {
                        let mut state = state.lock().unwrap();
                        state.connections += 1;
                        if let Ok(kept) = stream.try_clone() {
                            state.open.push(kept);
                        }
                    }
                    let state = Arc::clone(&state);
                    thread::spawn(move || serve(stream, state));
                }
            });
        }
    }

    fn reply(stream: &mut TcpStream, okay: bool, text: &str) {
        let status = if okay { "OKAY" } else { "FAIL" };
        let _ = stream.write_all(format!("{status}{:04x}{text}", text.len()).as_bytes());
    }

    fn list(devices: &[AdbDevice]) -> String {
        devices
            .iter()
            .map(|device| format!("{}\t{}\n", device.serial, device.state))
            .collect()
    }

    fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
        let mut length = [0u8; 4];
        if stream.read_exact(&mut length).is_err() {
            return;
        }
        let Some(length) = std::str::from_utf8(&length)
            .ok()
            .and_then(|digits| usize::from_str_radix(digits, 16).ok())
        else {
            return;
        };
        let mut request = vec![0u8; length];
        if stream.read_exact(&mut request).is_err() {
            return;
        }
        let request = String::from_utf8_lossy(&request).into_owned();
        let mut state_now = state.lock().unwrap();
        state_now.requests.push(request.clone());
        if request == "host:track-devices" {
            let devices = list(&state_now.devices);
            drop(state_now);
            let _ = stream.write_all(b"OKAY");
            let _ = stream.write_all(format!("{:04x}{devices}", devices.len()).as_bytes());
            // Held open until the instance ends.
            let mut sink = [0u8; 64];
            while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
            return;
        }
        if request == "host:devices" {
            let devices = list(&state_now.devices);
            drop(state_now);
            reply(&mut stream, true, &devices);
        } else if request == "host:version" {
            drop(state_now);
            reply(&mut stream, true, "0029");
        } else if let Some(serial) = request.strip_prefix("host:connect:") {
            let device_state = state_now.connected_state.clone();
            if !state_now.devices.iter().any(|d| d.serial == serial) {
                state_now.devices.push(AdbDevice {
                    serial: serial.into(),
                    state: device_state,
                });
            }
            drop(state_now);
            reply(&mut stream, true, &format!("connected to {serial}"));
        } else if let Some(serial) = request.strip_prefix("host:disconnect:") {
            let before = state_now.devices.len();
            state_now.devices.retain(|device| device.serial != serial);
            let removed = state_now.devices.len() != before;
            drop(state_now);
            if removed {
                reply(&mut stream, true, &format!("disconnected {serial}"));
            } else {
                reply(&mut stream, false, &format!("no such device '{serial}'"));
            }
        } else {
            drop(state_now);
            reply(&mut stream, false, "unknown host service");
        }
        let _ = stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr20_the_adb_server_port_follows_android_adb_server_port() {
        assert_eq!(adb_server_port(None), 5037);
        assert_eq!(adb_server_port(Some("5038")), 5038);
        assert_eq!(adb_server_port(Some(" 6000 ")), 6000);
        assert_eq!(adb_server_port(Some("0")), 5037, "not a port");
        assert_eq!(adb_server_port(Some("70000")), 5037, "not a port");
        assert_eq!(adb_server_port(Some("adb")), 5037, "not a number");
    }

    #[test]
    fn fr20_requests_are_length_prefixed_in_four_hex_digits() {
        let mut out = Vec::new();
        send_request(&mut out, "host:devices").unwrap();
        assert_eq!(out, b"000chost:devices");
    }

    #[test]
    fn fr20_okay_and_fail_answers_are_read() {
        let mut okay: &[u8] = b"OKAY0004abcd";
        read_status(&mut okay).unwrap();
        assert_eq!(read_block(&mut okay).unwrap(), "abcd");
        let mut fail: &[u8] = b"FAIL000eno such device";
        match read_status(&mut fail) {
            Err(AdbError::Fail(why)) => assert_eq!(why, "no such device"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        let mut junk: &[u8] = b"HTTP/1.1";
        assert!(matches!(
            read_status(&mut junk),
            Err(AdbError::Malformed(_))
        ));
    }

    #[test]
    fn fr20_a_device_list_is_parsed() {
        let devices = parse_devices("emulator-5554\tdevice\n127.0.0.1:58526\toffline\n\n");
        assert_eq!(
            devices,
            vec![
                AdbDevice {
                    serial: "emulator-5554".into(),
                    state: "device".into()
                },
                AdbDevice {
                    serial: "127.0.0.1:58526".into(),
                    state: "offline".into()
                },
            ]
        );
    }

    #[test]
    fn fr20_connect_answers_are_told_apart() {
        assert!(connected("connected to 127.0.0.1:58526"));
        assert!(connected("already connected to 127.0.0.1:58526"));
        assert!(!connected(
            "failed to connect to '127.0.0.1:58526': Connection refused"
        ));
    }

    #[test]
    fn fr20_state_names_round_trip() {
        for state in [
            AutoConnectState::Off,
            AutoConnectState::WaitingForServer,
            AutoConnectState::WaitingForAndroid,
            AutoConnectState::Attached,
            AutoConnectState::Detached,
        ] {
            assert_eq!(AutoConnectState::parse(state.as_str()), state);
        }
        assert_eq!(AutoConnectState::parse(""), AutoConnectState::Off);
    }

    #[test]
    fn fr20_the_mdns_name_carries_the_port() {
        assert_eq!(mdns_instance_name(58526), "adb-DSA-58526");
        assert_eq!(MDNS_SERVICE_TYPE, "_adb._tcp");
    }
}
