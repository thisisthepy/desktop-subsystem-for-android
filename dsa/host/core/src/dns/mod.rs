//! The guest's DNS, answered by the host's resolver.
//!
//! Every DNS query a program in the guest makes goes to the guest's control daemon, which
//! hands it to the host on [`DNS_PORT`] as a [`control::DnsQuery`]. The host answers it with
//! its operating system's resolver ([`HostResolver`]: `DnsQueryRaw` or `DnsQueryEx` on
//! Windows, the system's `getaddrinfo` and `res_query` on Linux and macOS), so the host's
//! hosts file, its name resolution policy (the NRPT on Windows), a VPN's split DNS and a DNS
//! filter apply to Android's apps as they do to the host's own programs. The guest never asks
//! the NAT gateway or a public DNS server itself while the host answers.
//!
//! [`DnsService`] turns one query into its answer:
//!
//! - It reads the query, and answers one it cannot take (no question, another opcode) with
//!   the matching error rather than dropping it.
//! - It asks the resolver on a thread of its own and waits [`RESOLVE_TIMEOUT`] at most; a
//!   resolver that fails or takes longer gets the program `SERVFAIL`, so a program in the guest
//!   never waits for an answer that does not come. At most [`MAX_IN_FLIGHT`] questions wait on
//!   the resolver at once; past that the answer is `SERVFAIL` at once.
//! - What the resolver gives (a whole response, or records) becomes the answer under the
//!   query's ID and question: TTLs, `NXDOMAIN`, an empty answer, `CNAME` chains and `AAAA`
//!   records as the resolver gave them. An answer too large for the program's UDP buffer goes
//!   out truncated, and the program asks again over TCP, where it gets all of it.
//! - A small cache ([`cache`]) keeps answers for their TTL (an empty answer or `NXDOMAIN` for
//!   its SOA's negative TTL), at most [`cache::MAX_TTL`], and counts their TTLs down. A failure
//!   is never kept.
//!
//! [`serve_connection`] serves one connection on the port, past the handshake: each query is
//! answered on a thread of its own, so a slow name holds back no other.

pub mod cache;
pub mod wire;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_protocol::control::{self, envelope::Body, Envelope};
use dsa_transport::{Connection, FrameSender, TransportError};

use cache::{Cache, CacheKey};
use wire::{Message, Question, Rcode, Record};

/// The vsock port the guest's control daemon hands DNS queries to the host on.
pub const DNS_PORT: u32 = 5605;

/// How long a question may wait for the host's resolver. Android's resolver waits five
/// seconds for a server before it tries the next, so the answer comes before that.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(4);

/// How many questions may wait on the resolver at once.
pub const MAX_IN_FLIGHT: usize = 64;

/// How long the port waits for the handshake.
pub const HANDSHAKE_WAIT: Duration = Duration::from_secs(10);

/// What the host's resolver gave for one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A whole DNS response, as a resolver that speaks the wire gives it (`DnsQueryRaw`,
    /// `res_query`). Its ID and flags do not matter; its question must be the one asked.
    Message(Vec<u8>),
    /// The records a resolver that does not speak the wire gave, with the response code.
    Records {
        rcode: Rcode,
        answers: Vec<Record>,
        authority: Vec<Record>,
    },
    /// The resolver could not answer, and why. The program gets `SERVFAIL`.
    Failed(String),
}

impl Resolution {
    /// The name does not exist.
    pub fn nxdomain() -> Resolution {
        Resolution::Records {
            rcode: Rcode::NxDomain,
            answers: Vec::new(),
            authority: Vec::new(),
        }
    }

    /// The name exists but has no records of the type asked.
    pub fn no_data() -> Resolution {
        Resolution::Records {
            rcode: Rcode::NoError,
            answers: Vec::new(),
            authority: Vec::new(),
        }
    }

    /// The records answer the question.
    pub fn answers(answers: Vec<Record>) -> Resolution {
        Resolution::Records {
            rcode: Rcode::NoError,
            answers,
            authority: Vec::new(),
        }
    }
}

/// The host operating system's resolver.
pub trait HostResolver: Send + Sync {
    /// Answers `question`, which a program asked in `query` (the whole message, for a
    /// resolver that takes one). Called on a thread of its own; it may block, and an answer
    /// that comes after [`RESOLVE_TIMEOUT`] still fills the cache.
    fn resolve(&self, question: &Question, query: &[u8]) -> Resolution;
}

/// Answers the guest's queries with a [`HostResolver`], through a cache.
pub struct DnsService {
    resolver: Arc<dyn HostResolver>,
    cache: Arc<Mutex<Cache>>,
    timeout: Duration,
    in_flight: Arc<AtomicUsize>,
}

impl DnsService {
    pub fn new(resolver: Arc<dyn HostResolver>) -> DnsService {
        DnsService {
            resolver,
            cache: Arc::new(Mutex::new(Cache::default())),
            timeout: RESOLVE_TIMEOUT,
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The same service, waiting `timeout` for the resolver.
    pub fn with_timeout(mut self, timeout: Duration) -> DnsService {
        self.timeout = timeout;
        self
    }

    /// The answer to `query`, a DNS message a program in the guest sent; `tcp` says whether
    /// it came over TCP, where an answer of any size fits. `None` only for bytes too short to
    /// carry a DNS header, which no answer can be addressed to.
    pub fn answer(&self, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        let message = match wire::parse(query) {
            Ok(message) => message,
            Err(_) => return format_error(query),
        };
        if message.is_response() {
            return None;
        }
        if message.opcode() != 0 {
            return Some(wire::error_answer(&message, Rcode::NotImp));
        }
        let [question] = message.questions.as_slice() else {
            return Some(wire::error_answer(&message, Rcode::FormErr));
        };
        let limit = if tcp {
            wire::MAX_MESSAGE_LEN
        } else {
            wire::udp_limit(&message)
        };
        let key = CacheKey::of(question, &message);
        if let Some(cached) = self.lock_cache().get(&key) {
            return Some(wire::answer_for(&message, &cached, limit));
        }
        let response = match self.ask(question, query, key) {
            Ok(response) => response,
            Err(why) => {
                eprintln!(
                    "dns: {} (type {}) is answered SERVFAIL: {why}",
                    question.name, question.qtype
                );
                return Some(wire::error_answer(&message, Rcode::ServFail));
            }
        };
        Some(wire::answer_for(&message, &response, limit))
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Asks the resolver, waiting [`DnsService::timeout`] at most.
    fn ask(&self, question: &Question, query: &[u8], key: CacheKey) -> Result<Message, String> {
        if self.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(format!(
                "{MAX_IN_FLIGHT} questions already wait on the host's resolver"
            ));
        }
        let (done, wait) = mpsc::channel();
        let (resolver, cache, in_flight) = (
            Arc::clone(&self.resolver),
            Arc::clone(&self.cache),
            Arc::clone(&self.in_flight),
        );
        let (question, query) = (question.clone(), query.to_vec());
        let spawned = thread::Builder::new()
            .name("dsa-dns-resolve".into())
            .spawn(move || {
                let resolved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    resolver.resolve(&question, &query)
                }))
                .unwrap_or_else(|_| Resolution::Failed("the host's resolver failed".into()));
                let response = response_of(&question, resolved);
                if let Ok(response) = &response {
                    cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .put(key, response);
                }
                in_flight.fetch_sub(1, Ordering::SeqCst);
                let _ = done.send(response);
            });
        if let Err(why) = spawned {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return Err(format!("no thread could ask the host's resolver: {why}"));
        }
        match wait.recv_timeout(self.timeout) {
            Ok(response) => response,
            Err(_) => Err(format!(
                "the host's resolver did not answer within {} ms",
                self.timeout.as_millis()
            )),
        }
    }
}

/// The response `resolved` makes for `question`, or why it makes none.
fn response_of(question: &Question, resolved: Resolution) -> Result<Message, String> {
    match resolved {
        Resolution::Message(bytes) => {
            let response = wire::parse(&bytes)
                .map_err(|e| format!("the host's resolver gave an unreadable answer ({e})"))?;
            match response.questions.as_slice() {
                [] => {}
                [asked]
                    if asked.name.same(&question.name)
                        && asked.qtype == question.qtype
                        && asked.qclass == question.qclass => {}
                _ => return Err("the host's resolver answered another question".into()),
            }
            if response.rcode == Rcode::ServFail {
                return Err("the host's resolver answered SERVFAIL".into());
            }
            Ok(response)
        }
        Resolution::Records {
            rcode,
            answers,
            authority,
        } => {
            if rcode == Rcode::ServFail {
                return Err("the host's resolver failed".into());
            }
            Ok(wire::response_with(rcode, answers, authority))
        }
        Resolution::Failed(why) => Err(why),
    }
}

/// `FORMERR` for a query that has a header but cannot be read further; `None` without one.
fn format_error(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < wire::HEADER_LEN || query[2] & 0x80 != 0 {
        return None;
    }
    let mut answer = query[..wire::HEADER_LEN].to_vec();
    // QR set, the opcode and RD kept, everything else cleared; FORMERR; no sections.
    answer[2] = 0x80 | (query[2] & 0x79);
    answer[3] = 0x80 | Rcode::FormErr.code();
    answer[4..].fill(0);
    Some(answer)
}

/// Serves one connection on the DNS port, after the handshake, until it closes or `stop` is
/// set: answers every [`control::DnsQuery`] with a [`control::DnsAnswer`] of the same id.
/// Each query is answered on a thread of its own when the connection can send from another
/// thread, in turn otherwise.
pub fn serve_connection(
    connection: &mut dyn Connection,
    service: &Arc<DnsService>,
    stop: &AtomicBool,
) -> TransportError {
    let sender: Option<Arc<Mutex<Box<dyn FrameSender>>>> = connection
        .try_clone_sender()
        .map(|sender| Arc::new(Mutex::new(sender)));
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
        let query = match dsa_protocol::decode(&frame) {
            Ok(Envelope {
                body: Some(Body::DnsQuery(query)),
            }) => query,
            Ok(_) => {
                eprintln!("dns: dropping a message other than a DNS query on the DNS port");
                continue;
            }
            Err(error) => {
                eprintln!("dns: dropping an unreadable frame on the DNS port ({error})");
                continue;
            }
        };
        match &sender {
            Some(sender) => {
                let (service, sender) = (Arc::clone(service), Arc::clone(sender));
                let spawned =
                    thread::Builder::new()
                        .name("dsa-dns-query".into())
                        .spawn(move || {
                            if let Some(frame) = answer_frame(&service, &query) {
                                // A lost connection shows up on the reading side.
                                let _ = sender
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .send(&frame);
                            }
                        });
                if let Err(why) = spawned {
                    eprintln!("dns: a query is left unanswered, no thread could take it: {why}");
                }
            }
            None => {
                if let Some(frame) = answer_frame(service, &query) {
                    if let Err(error) = connection.send(&frame) {
                        return error;
                    }
                }
            }
        }
    }
}

fn answer_frame(service: &DnsService, query: &control::DnsQuery) -> Option<Vec<u8>> {
    let message = service.answer(&query.message, query.tcp)?;
    Some(dsa_protocol::encode(&Envelope {
        body: Some(Body::DnsAnswer(control::DnsAnswer {
            id: query.id,
            message,
        })),
    }))
}
