//! The guest's DNS, answered by the host's resolver.
//!
//! Android's resolver (netd's DnsResolver) asks the DNS servers of its network, which DHCP
//! gives it. While the host answers, every DNS query the guest sends to port 53 is redirected
//! to this stub instead, on the guest's loopback address ([`STUB_PORT`], UDP and TCP, IPv4
//! and IPv6): init's packet filter does that (`dns_redirect`, switched by [`PROPERTY`]). The
//! stub hands each query to the host on the DNS port ([`PORT`]) over vsock, past the same
//! handshake as the control channel, and sends the host's answer back to the program that
//! asked. The host answers with its operating system's resolver, so its hosts file, its name
//! resolution policy, a VPN's split DNS and a DNS filter apply to Android's apps as they do to
//! the host's own programs, and the guest never asks the NAT gateway or a public server
//! itself. Android's private DNS is left off by default for the same reason: its DNS-over-TLS
//! would go to the network's servers around the host (the agent sets the default).
//!
//! It stays on in advanced networking too, where the guest is on the host's LAN: the host's
//! resolver knows the LAN's names as well (it asks the same router), and a VPN or a DNS
//! filter on the host still applies to Android's apps.
//!
//! Nothing hangs a program that asks:
//!
//! - A query the host does not answer within [`QUERY_TIMEOUT`] is answered `SERVFAIL`.
//! - While the connection to the host is lost, every query is answered `SERVFAIL` at once.
//!   When it stays lost for [`FALLBACK_AFTER`], the redirect is switched off, and Android's
//!   resolver asks its network's DNS servers again, as it did before the host answered; it is
//!   switched on again as soon as the host answers once more.
//! - A stub that cannot listen never switches the redirect on, so the guest keeps its
//!   network's DNS servers.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::control::{self, envelope::Body, Envelope};
use dsa_transport::{Connection, FrameSender, TransportError};

/// The host's vsock port for DNS queries; the host serves it on the same number.
pub const PORT: u32 = 5605;

/// The port the stub listens on, on the guest's loopback addresses, where the redirect sends
/// every DNS query. Not 53, which only root may bind, and not 5353, which is mDNS's.
pub const STUB_PORT: u16 = 5553;

/// The property that switches the redirect: `on` while the host answers, `off` otherwise.
pub const PROPERTY: &str = "vendor.dsa.dns";

/// How long a query waits for the host's answer.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the connection to the host may stay lost before the redirect is switched off.
pub const FALLBACK_AFTER: Duration = Duration::from_secs(5);

/// How many queries may wait for the host at once; past that, `SERVFAIL` at once.
pub const MAX_WAITING: usize = 256;

/// How long a TCP connection to the stub may sit idle.
const TCP_IDLE: Duration = Duration::from_secs(10);

/// The longest DNS message.
const MAX_MESSAGE_LEN: usize = 65535;

/// Opens a connection to the host's DNS port and passes the handshake.
pub type DnsConnector = dyn Fn() -> Result<Box<dyn Connection>, String> + Send + Sync;

/// The `SERVFAIL` answer to `query`, for a program whose query the host could not answer:
/// the query's ID, opcode, flags and question, with `QR` and `RA` set. `None` for bytes too
/// short to carry a DNS header, or a message that is itself an answer.
pub fn servfail(query: &[u8]) -> Option<Vec<u8>> {
    const HEADER: usize = 12;
    if query.len() < HEADER || query[2] & 0x80 != 0 {
        return None;
    }
    let questions = u16::from_be_bytes([query[4], query[5]]);
    // The question goes back only when there is exactly one and it can be found whole.
    let question_end = (questions == 1).then(|| question_end(query)).flatten();
    let mut answer = Vec::with_capacity(question_end.unwrap_or(HEADER));
    answer.extend_from_slice(&query[..HEADER]);
    answer[2] = 0x80 | (query[2] & 0x79);
    answer[3] = 0x80 | (query[3] & 0x10) | 2;
    answer[4..HEADER].fill(0);
    if let Some(end) = question_end {
        answer[5] = 1;
        answer.extend_from_slice(&query[HEADER..end]);
    }
    Some(answer)
}

/// Where the first question of `query` ends, if it can be read.
fn question_end(query: &[u8]) -> Option<usize> {
    let mut at = 12;
    loop {
        let len = *query.get(at)? as usize;
        match len & 0xc0 {
            0x00 if len == 0 => {
                at += 1;
                break;
            }
            0x00 => at += 1 + len,
            0xc0 => {
                at += 2;
                break;
            }
            _ => return None,
        }
    }
    let end = at + 4;
    (end <= query.len()).then_some(end)
}

/// One query waiting for the host's answer.
type Waiter = Sender<Vec<u8>>;

#[derive(Default)]
struct TunnelState {
    sender: Option<Box<dyn FrameSender>>,
    waiting: HashMap<u32, Waiter>,
    next_id: u32,
}

/// The connection to the host's DNS port, shared by every query the stub takes.
pub struct Tunnel {
    connect: Box<DnsConnector>,
    switch: Box<dyn Fn(bool) + Send + Sync>,
    state: Mutex<TunnelState>,
    query_timeout: Duration,
    fallback_after: Duration,
    waiting: AtomicUsize,
}

impl Tunnel {
    /// A tunnel that connects with `connect` and switches the redirect with `switch` (`true`
    /// while the host answers).
    pub fn new(connect: Box<DnsConnector>, switch: Box<dyn Fn(bool) + Send + Sync>) -> Tunnel {
        Tunnel {
            connect,
            switch,
            state: Mutex::new(TunnelState::default()),
            query_timeout: QUERY_TIMEOUT,
            fallback_after: FALLBACK_AFTER,
            waiting: AtomicUsize::new(0),
        }
    }

    /// The same tunnel with other waits, for tests.
    pub fn with_timeouts(mut self, query: Duration, fallback_after: Duration) -> Tunnel {
        self.query_timeout = query;
        self.fallback_after = fallback_after;
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TunnelState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the host is connected now.
    pub fn connected(&self) -> bool {
        self.lock().sender.is_some()
    }

    /// Hands `query` to the host and waits for its answer. `None` when the host is not
    /// connected, too many queries wait already, or the answer did not come in time.
    pub fn ask(&self, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= MAX_WAITING {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let answer = self.ask_now(query, tcp);
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        answer
    }

    fn ask_now(&self, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        let (done, wait) = mpsc::channel();
        let id = {
            let mut state = self.lock();
            state.sender.as_ref()?;
            let mut id = state.next_id;
            while state.waiting.contains_key(&id) {
                id = id.wrapping_add(1);
            }
            state.next_id = id.wrapping_add(1);
            state.waiting.insert(id, done);
            let frame = dsa_protocol::encode(&Envelope {
                body: Some(Body::DnsQuery(control::DnsQuery {
                    id,
                    message: query.to_vec(),
                    tcp,
                })),
            });
            let sent = state.sender.as_mut().map(|sender| sender.send(&frame));
            if !matches!(sent, Some(Ok(()))) {
                state.waiting.remove(&id);
                return None;
            }
            id
        };
        let answer = wait.recv_timeout(self.query_timeout).ok();
        self.lock().waiting.remove(&id);
        answer
    }

    /// Keeps the connection to the host until `stop` is set: connects, switches the redirect
    /// on, delivers the host's answers, and connects again when the connection is lost,
    /// switching the redirect off when it stays lost.
    pub fn run(&self, stop: &AtomicBool, say: &dyn Fn(&str)) {
        let mut lost_since: Option<Instant> = Some(Instant::now());
        // Unknown at first: a daemon that restarted within the boot may find the redirect
        // still on from before, so it is switched off too when the host does not come.
        let mut switched: Option<bool> = None;
        let mut delay = Duration::from_millis(100);
        let mut reported = false;
        while !stop.load(Ordering::Relaxed) {
            match (self.connect)() {
                Ok(mut connection) => {
                    let Some(sender) = connection.try_clone_sender() else {
                        say("the DNS connection to the host cannot send from a second thread, so the guest keeps its network's DNS servers");
                        return;
                    };
                    self.lock().sender = Some(sender);
                    delay = Duration::from_millis(100);
                    reported = false;
                    if switched != Some(true) {
                        (self.switch)(true);
                        switched = Some(true);
                    }
                    say("the guest's DNS queries go to the host's resolver");
                    let why = self.deliver(connection.as_mut(), stop);
                    self.drop_connection();
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    say(&format!(
                        "lost the host's DNS connection ({why}); connecting again"
                    ));
                    lost_since = Some(Instant::now());
                }
                Err(why) => {
                    if !reported {
                        say(&format!("the host's DNS port cannot be reached yet: {why}"));
                        reported = true;
                    }
                }
            }
            if switched != Some(false)
                && lost_since.is_some_and(|since| since.elapsed() >= self.fallback_after)
            {
                (self.switch)(false);
                switched = Some(false);
                say("the host's DNS stays unreachable: the guest asks its network's DNS servers until it is back");
            }
            let deadline = Instant::now() + delay;
            while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(20).min(deadline - Instant::now()));
            }
            delay = (delay * 2).min(Duration::from_secs(2));
        }
        self.drop_connection();
        if switched == Some(true) {
            (self.switch)(false);
        }
    }

    /// Hands every answer on `connection` to the query waiting for it, until the connection
    /// is lost or `stop` is set.
    fn deliver(&self, connection: &mut dyn Connection, stop: &AtomicBool) -> TransportError {
        loop {
            if stop.load(Ordering::Relaxed) {
                connection.close();
                return TransportError::Closed;
            }
            let frame = match connection.recv(Some(Duration::from_millis(200))) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => continue,
                Err(error) => return error,
            };
            if let Ok(Envelope {
                body: Some(Body::DnsAnswer(answer)),
            }) = dsa_protocol::decode(&frame)
            {
                if let Some(waiter) = self.lock().waiting.remove(&answer.id) {
                    let _ = waiter.send(answer.message);
                }
            }
        }
    }

    /// Forgets the connection; every query still waiting is answered `SERVFAIL` at once.
    fn drop_connection(&self) {
        let mut state = self.lock();
        state.sender = None;
        state.waiting.clear();
    }
}

/// The answer a program gets to `query`: the host's, or `SERVFAIL`.
pub fn answer(tunnel: &Tunnel, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
    tunnel.ask(query, tcp).or_else(|| servfail(query))
}

/// The stub's sockets.
pub struct Stub {
    pub udp: Vec<UdpSocket>,
    pub tcp: Vec<TcpListener>,
}

impl Stub {
    /// Listens on `port` on the guest's IPv4 and IPv6 loopback addresses, over UDP and TCP.
    /// The IPv6 sockets are optional, as a guest without IPv6 has no `::1`; the IPv4 ones are
    /// not.
    pub fn bind(port: u16) -> Result<Stub, String> {
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
        let mut stub = Stub {
            udp: vec![UdpSocket::bind(v4).map_err(|e| format!("UDP {v4}: {e}"))?],
            tcp: vec![TcpListener::bind(v4).map_err(|e| format!("TCP {v4}: {e}"))?],
        };
        if let Ok(socket) = UdpSocket::bind(v6) {
            stub.udp.push(socket);
        }
        if let Ok(listener) = TcpListener::bind(v6) {
            stub.tcp.push(listener);
        }
        Ok(stub)
    }

    /// Serves every socket on threads of its own, with `tunnel`.
    pub fn serve(self, tunnel: Arc<Tunnel>, say: Arc<dyn Fn(&str) + Send + Sync>) {
        for socket in self.udp {
            let (tunnel, say) = (Arc::clone(&tunnel), Arc::clone(&say));
            let _ = thread::Builder::new()
                .name("dsa-dns-udp".into())
                .spawn(move || serve_udp(socket, tunnel, say.as_ref()));
        }
        for listener in self.tcp {
            let (tunnel, say) = (Arc::clone(&tunnel), Arc::clone(&say));
            let _ = thread::Builder::new()
                .name("dsa-dns-tcp".into())
                .spawn(move || serve_tcp(listener, tunnel, say.as_ref()));
        }
    }
}

/// Answers every datagram on `socket`, each on a thread of its own.
pub fn serve_udp(socket: UdpSocket, tunnel: Arc<Tunnel>, say: &(dyn Fn(&str) + Send + Sync)) {
    let reply = match socket.try_clone() {
        Ok(reply) => Arc::new(reply),
        Err(why) => {
            say(&format!("the DNS stub's UDP socket cannot reply: {why}"));
            return;
        }
    };
    let mut buffer = vec![0u8; MAX_MESSAGE_LEN];
    loop {
        let (len, from) = match socket.recv_from(&mut buffer) {
            Ok(received) => received,
            Err(why) => {
                if why.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                say(&format!("the DNS stub stopped reading UDP queries: {why}"));
                return;
            }
        };
        let query = buffer[..len].to_vec();
        let (tunnel, to) = (Arc::clone(&tunnel), Arc::clone(&reply));
        let spawned = thread::Builder::new()
            .name("dsa-dns-query".into())
            .spawn(move || {
                if let Some(answer) = answer(&tunnel, &query, false) {
                    let _ = to.send_to(&answer, from);
                }
            });
        if spawned.is_err() {
            if let Some(answer) = servfail(&buffer[..len]) {
                let _ = reply.send_to(&answer, from);
            }
        }
    }
}

/// Answers every connection on `listener`, each on a thread of its own: two length bytes and a
/// message, any number of times (RFC 7766).
pub fn serve_tcp(listener: TcpListener, tunnel: Arc<Tunnel>, say: &(dyn Fn(&str) + Send + Sync)) {
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(why) => {
                if why.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                say(&format!("the DNS stub stopped taking TCP queries: {why}"));
                return;
            }
        };
        let tunnel = Arc::clone(&tunnel);
        let _ = thread::Builder::new()
            .name("dsa-dns-tcp-query".into())
            .spawn(move || serve_stream(stream, &tunnel));
    }
}

fn serve_stream(mut stream: TcpStream, tunnel: &Tunnel) {
    let _ = stream.set_read_timeout(Some(TCP_IDLE));
    loop {
        let mut len = [0u8; 2];
        if stream.read_exact(&mut len).is_err() {
            return;
        }
        let mut query = vec![0u8; u16::from_be_bytes(len) as usize];
        if stream.read_exact(&mut query).is_err() {
            return;
        }
        let Some(answer) = answer(tunnel, &query, true) else {
            return;
        };
        let Ok(answer_len) = u16::try_from(answer.len()) else {
            return;
        };
        let mut out = Vec::with_capacity(2 + answer.len());
        out.extend_from_slice(&answer_len.to_be_bytes());
        out.extend_from_slice(&answer);
        if stream.write_all(&out).is_err() {
            return;
        }
    }
}
