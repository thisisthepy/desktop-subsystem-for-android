//! The host's answers to the guest's DNS queries: the DNS wire handling, the cache, the
//! timeouts, and the DNS port's frames, with a fake resolver in place of the operating
//! system's.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::dns::cache::{keep_for, Cache, CacheKey, MAX_TTL, NEGATIVE_TTL};
use dsa_host_core::dns::wire::{
    self, Edns, Message, Name, Rcode, Rdata, Record, CLASS_IN, TYPE_A, TYPE_AAAA, TYPE_CNAME,
    TYPE_MX, TYPE_SOA, TYPE_TXT,
};
use dsa_host_core::dns::{serve_connection, DnsService, HostResolver, Resolution};
use dsa_protocol::control::{self, envelope::Body, Envelope};
use dsa_transport::memory;
use dsa_transport::{Connector, Listener};

fn name(text: &str) -> Name {
    Name::from_dotted(text).unwrap()
}

fn query_bytes(id: u16, text: &str, qtype: u16) -> Vec<u8> {
    wire::encode(&wire::query(id, &name(text), qtype))
}

/// A resolver that knows the names it is given, refuses the blocked ones, and counts what it
/// was asked.
#[derive(Default)]
struct FakeResolver {
    names: Vec<(Name, Resolution)>,
    asked: AtomicUsize,
    delay: Option<Duration>,
}

impl FakeResolver {
    fn with(mut self, text: &str, resolution: Resolution) -> Self {
        self.names.push((name(text), resolution));
        self
    }
}

impl HostResolver for FakeResolver {
    fn resolve(&self, question: &wire::Question, _query: &[u8]) -> Resolution {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if let Some(delay) = self.delay {
            std::thread::sleep(delay);
        }
        self.names
            .iter()
            .find(|(known, _)| known.same(&question.name))
            .map(|(_, resolution)| resolution.clone())
            .unwrap_or_else(Resolution::nxdomain)
    }
}

fn answer(service: &DnsService, query: &[u8], tcp: bool) -> Message {
    wire::parse(&service.answer(query, tcp).expect("an answer")).expect("a readable answer")
}

#[test]
fn fr28_a_query_reads_back_as_it_was_written() {
    let bytes = query_bytes(0x1234, "Host-Only.Example", TYPE_AAAA);
    let message = wire::parse(&bytes).unwrap();
    assert_eq!(message.id, 0x1234);
    assert!(message.recursion_desired());
    assert!(!message.is_response());
    assert_eq!(message.questions.len(), 1);
    assert_eq!(message.questions[0].name.to_dotted(), "Host-Only.Example");
    assert_eq!(message.questions[0].qtype, TYPE_AAAA);
    assert_eq!(message.questions[0].qclass, CLASS_IN);
    assert!(message.questions[0].name.same(&name("host-only.example")));
}

#[test]
fn fr28_names_are_compressed_and_compressed_names_are_followed() {
    let owner = name("www.example.com");
    let target = name("cdn.example.com");
    let mut response = wire::response_with(
        Rcode::NoError,
        vec![
            Record::with_name(&owner, TYPE_CNAME, 60, target.clone()),
            Record::a(&target, 30, Ipv4Addr::new(192, 0, 2, 7)),
        ],
        Vec::new(),
    );
    response.questions = wire::query(9, &owner, TYPE_A).questions;
    let bytes = wire::encode(&response);
    // The header, the question (17 + 4), the CNAME with its owner as a pointer and its target
    // as "cdn" and a pointer (2 + 10 + 6), and the A record under a pointer (2 + 10 + 4).
    assert_eq!(bytes.len(), 12 + 21 + 18 + 16);
    let read = wire::parse(&bytes).unwrap();
    assert_eq!(read.answers, response.answers);
    assert_eq!(read.questions, response.questions);
}

#[test]
fn fr28_a_message_with_a_pointer_loop_or_a_cut_is_refused() {
    let mut bytes = query_bytes(1, "a.example", TYPE_A);
    // The question's name becomes a pointer to itself.
    bytes[12] = 0xc0;
    bytes[13] = 12;
    assert!(wire::parse(&bytes).is_err());
    let bytes = query_bytes(1, "a.example", TYPE_A);
    for cut in 0..bytes.len() {
        assert!(wire::parse(&bytes[..cut]).is_err(), "cut at {cut}");
    }
    // A record whose stated length does not match its parts.
    let mut response = wire::response_with(
        Rcode::NoError,
        vec![Record::with_name(
            &name("a.example"),
            TYPE_CNAME,
            5,
            name("b.example"),
        )],
        Vec::new(),
    );
    response.questions = wire::query(1, &name("a.example"), TYPE_A).questions;
    let mut bytes = wire::encode(&response);
    let rdlength_at = bytes
        .windows(4)
        .rposition(|w| w == [0, 0, 0, 5])
        .map(|at| at + 4)
        .unwrap();
    bytes[rdlength_at + 1] += 1;
    bytes.push(0);
    assert!(wire::parse(&bytes).is_err());
}

#[test]
fn fr28_the_answer_carries_the_querys_id_question_and_records_with_their_ttls() {
    let resolver = Arc::new(FakeResolver::default().with(
        "host-only.example",
        Resolution::answers(vec![Record::a(
            &name("host-only.example"),
            1234,
            Ipv4Addr::new(198, 51, 100, 7),
        )]),
    ));
    let service = DnsService::new(resolver);
    let response = answer(
        &service,
        &query_bytes(0xbeef, "HOST-ONLY.example", TYPE_A),
        false,
    );
    assert_eq!(response.id, 0xbeef);
    assert!(response.is_response());
    assert!(response.recursion_desired());
    assert_eq!(response.rcode, Rcode::NoError);
    assert_eq!(response.questions[0].name.to_dotted(), "HOST-ONLY.example");
    assert_eq!(response.answers.len(), 1);
    assert_eq!(response.answers[0].ttl, 1234);
    // The cached copy keeps the resolver's TTL too.
    let again = answer(
        &service,
        &query_bytes(1, "host-only.example", TYPE_A),
        false,
    );
    assert!(again.answers[0].ttl <= 1234 && again.answers[0].ttl >= 1233);
    assert_eq!(response.answers[0].data, Rdata::Raw(vec![198, 51, 100, 7]));
}

#[test]
fn fr28_a_name_the_host_blocks_is_nxdomain_and_a_failure_is_servfail() {
    let resolver = Arc::new(
        FakeResolver::default()
            .with("blocked.example", Resolution::nxdomain())
            .with("broken.example", Resolution::Failed("no network".into()))
            .with("v4-only.example", Resolution::no_data()),
    );
    let service = DnsService::new(resolver);
    assert_eq!(
        answer(&service, &query_bytes(1, "blocked.example", TYPE_A), false).rcode,
        Rcode::NxDomain
    );
    assert_eq!(
        answer(&service, &query_bytes(2, "broken.example", TYPE_A), false).rcode,
        Rcode::ServFail
    );
    let empty = answer(
        &service,
        &query_bytes(3, "v4-only.example", TYPE_AAAA),
        false,
    );
    assert_eq!(empty.rcode, Rcode::NoError);
    assert!(empty.answers.is_empty());
}

#[test]
fn fr28_a_whole_response_from_the_resolver_keeps_its_cname_chain_and_aaaa_records() {
    let owner = name("www.example.net");
    let middle = name("edge.example.net");
    let last = name("node.cdn.example");
    let mut upstream = wire::response_with(
        Rcode::NoError,
        vec![
            Record::with_name(&owner, TYPE_CNAME, 300, middle.clone()),
            Record::with_name(&middle, TYPE_CNAME, 120, last.clone()),
            Record::aaaa(&last, 20, "2001:db8::7".parse::<Ipv6Addr>().unwrap()),
        ],
        Vec::new(),
    );
    upstream.id = 777;
    upstream.questions = wire::query(777, &owner, TYPE_AAAA).questions;
    let resolver = Arc::new(FakeResolver::default().with(
        "www.example.net",
        Resolution::Message(wire::encode(&upstream)),
    ));
    let service = DnsService::new(resolver);
    let response = answer(
        &service,
        &query_bytes(5, "www.example.net", TYPE_AAAA),
        false,
    );
    assert_eq!(response.id, 5);
    assert_eq!(response.answers, upstream.answers);
}

#[test]
fn fr28_a_response_to_another_question_is_servfail() {
    let mut upstream = wire::response_with(Rcode::NoError, Vec::new(), Vec::new());
    upstream.questions = wire::query(1, &name("other.example"), TYPE_A).questions;
    let resolver = Arc::new(FakeResolver::default().with(
        "asked.example",
        Resolution::Message(wire::encode(&upstream)),
    ));
    let service = DnsService::new(resolver);
    assert_eq!(
        answer(&service, &query_bytes(1, "asked.example", TYPE_A), false).rcode,
        Rcode::ServFail
    );
}

#[test]
fn fr28_a_large_udp_answer_is_truncated_and_comes_whole_over_tcp() {
    let owner = name("many.example");
    let records: Vec<Record> = (0..60)
        .map(|i| Record::a(&owner, 60, Ipv4Addr::new(203, 0, 113, i)))
        .collect();
    let resolver =
        Arc::new(FakeResolver::default().with("many.example", Resolution::answers(records)));
    let service = DnsService::new(resolver);
    let query = query_bytes(8, "many.example", TYPE_A);
    let udp = service.answer(&query, false).unwrap();
    assert!(udp.len() <= wire::CLASSIC_UDP_LIMIT);
    let udp = wire::parse(&udp).unwrap();
    assert!(udp.truncated());
    assert_eq!(udp.questions.len(), 1);
    assert!(udp.answers.is_empty());
    let tcp = answer(&service, &query, true);
    assert!(!tcp.truncated());
    assert_eq!(tcp.answers.len(), 60);
}

#[test]
fn fr28_an_edns_query_gets_a_larger_udp_answer_and_an_edns_record() {
    let owner = name("many.example");
    let records: Vec<Record> = (0..60)
        .map(|i| Record::a(&owner, 60, Ipv4Addr::new(203, 0, 113, i)))
        .collect();
    let resolver =
        Arc::new(FakeResolver::default().with("many.example", Resolution::answers(records)));
    let service = DnsService::new(resolver);
    let mut query = wire::query(4, &owner, TYPE_A);
    query.edns = Some(Edns {
        udp_size: 4096,
        dnssec_ok: false,
    });
    let response = answer(&service, &wire::encode(&query), false);
    assert!(!response.truncated());
    assert_eq!(response.answers.len(), 60);
    assert_eq!(response.edns.map(|e| e.udp_size), Some(wire::EDNS_UDP_SIZE));
}

#[test]
fn fr28_a_resolver_that_takes_too_long_gets_servfail_without_holding_the_program() {
    let resolver = Arc::new(FakeResolver {
        delay: Some(Duration::from_secs(5)),
        ..FakeResolver::default()
    });
    let service = DnsService::new(resolver).with_timeout(Duration::from_millis(100));
    let started = Instant::now();
    let response = answer(&service, &query_bytes(6, "slow.example", TYPE_A), false);
    assert_eq!(response.rcode, Rcode::ServFail);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn fr28_answers_are_cached_and_failures_are_not() {
    let resolver = Arc::new(
        FakeResolver::default()
            .with(
                "cached.example",
                Resolution::answers(vec![Record::a(
                    &name("cached.example"),
                    60,
                    Ipv4Addr::new(192, 0, 2, 1),
                )]),
            )
            .with("broken.example", Resolution::Failed("no network".into())),
    );
    let service = DnsService::new(Arc::clone(&resolver) as Arc<dyn HostResolver>);
    for id in 0..3 {
        answer(&service, &query_bytes(id, "cached.example", TYPE_A), false);
        answer(&service, &query_bytes(id, "broken.example", TYPE_A), false);
    }
    // One question for the cached name, one for each attempt at the broken one.
    assert_eq!(resolver.asked.load(Ordering::SeqCst), 4);
}

#[test]
fn fr28_a_cached_answers_ttl_counts_down_and_expires() {
    let owner = name("ttl.example");
    let query = wire::query(1, &owner, TYPE_A);
    let key = CacheKey::of(&query.questions[0], &query);
    let response = wire::response_with(
        Rcode::NoError,
        vec![Record::a(&owner, 100, Ipv4Addr::new(192, 0, 2, 9))],
        Vec::new(),
    );
    let mut cache = Cache::default();
    let start = Instant::now();
    cache.put_at(key.clone(), &response, start);
    let aged = cache.get_at(&key, start + Duration::from_secs(40)).unwrap();
    assert_eq!(aged.answers[0].ttl, 60);
    assert!(cache
        .get_at(&key, start + Duration::from_secs(100))
        .is_none());
    assert!(cache.is_empty());
}

#[test]
fn fr28_negative_answers_are_kept_for_their_soas_negative_ttl() {
    let soa = Record {
        name: name("example"),
        rtype: TYPE_SOA,
        class: CLASS_IN,
        ttl: 900,
        data: Rdata::Soa {
            mname: name("ns.example"),
            rname: name("hostmaster.example"),
            serial: 1,
            refresh: 2,
            retry: 3,
            expire: 4,
            minimum: 45,
        },
    };
    let nxdomain = wire::response_with(Rcode::NxDomain, Vec::new(), vec![soa]);
    assert_eq!(keep_for(&nxdomain), Some(45));
    let bare = wire::response_with(Rcode::NxDomain, Vec::new(), Vec::new());
    assert_eq!(keep_for(&bare), Some(NEGATIVE_TTL));
    let failed = wire::response_with(Rcode::ServFail, Vec::new(), Vec::new());
    assert_eq!(keep_for(&failed), None);
    let zero = wire::response_with(
        Rcode::NoError,
        vec![Record::a(&name("z.example"), 0, Ipv4Addr::LOCALHOST)],
        Vec::new(),
    );
    assert_eq!(keep_for(&zero), None);
    let long = wire::response_with(
        Rcode::NoError,
        vec![Record::a(&name("l.example"), 86_400, Ipv4Addr::LOCALHOST)],
        Vec::new(),
    );
    assert_eq!(keep_for(&long), Some(MAX_TTL));
}

#[test]
fn fr28_other_record_types_go_through_whole() {
    let owner = name("example.org");
    let txt = Record {
        name: owner.clone(),
        rtype: TYPE_TXT,
        class: CLASS_IN,
        ttl: 60,
        data: Rdata::Raw(b"\x0bhello world".to_vec()),
    };
    let mx = Record {
        name: owner.clone(),
        rtype: TYPE_MX,
        class: CLASS_IN,
        ttl: 60,
        data: Rdata::Mx {
            preference: 10,
            exchange: name("mail.example.org"),
        },
    };
    let mut upstream = wire::response_with(Rcode::NoError, vec![txt, mx], Vec::new());
    upstream.questions = wire::query(1, &owner, TYPE_TXT).questions;
    let read = wire::parse(&wire::encode(&upstream)).unwrap();
    assert_eq!(read.answers, upstream.answers);
}

#[test]
fn fr28_a_query_that_cannot_be_read_gets_formerr_and_a_non_query_gets_nothing() {
    let service = DnsService::new(Arc::new(FakeResolver::default()));
    let mut bytes = query_bytes(0x4242, "x.example", TYPE_A);
    bytes.truncate(wire::HEADER_LEN + 3);
    let answer = wire::parse(&service.answer(&bytes, false).unwrap()).unwrap();
    assert_eq!(answer.id, 0x4242);
    assert_eq!(answer.rcode, Rcode::FormErr);
    assert!(service.answer(&bytes[..5], false).is_none());
    let mut response = wire::query(1, &name("x.example"), TYPE_A);
    response.flags |= 0x8000;
    assert!(service.answer(&wire::encode(&response), false).is_none());
}

#[test]
fn fr28_the_dns_port_answers_each_query_with_its_id() {
    let resolver = Arc::new(FakeResolver::default().with(
        "host-only.example",
        Resolution::answers(vec![Record::a(
            &name("host-only.example"),
            60,
            Ipv4Addr::new(198, 51, 100, 7),
        )]),
    ));
    let service = Arc::new(DnsService::new(resolver));
    let (mut listener, mut connector) = memory::pair();
    let stop = Arc::new(AtomicBool::new(false));
    let host = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut connection = listener.accept(Duration::from_secs(5)).unwrap();
            serve_connection(connection.as_mut(), &service, &stop)
        })
    };
    let mut guest = connector.connect(Duration::from_secs(5)).unwrap();
    for (id, text) in [(41u32, "host-only.example"), (42, "unknown.example")] {
        let frame = dsa_protocol::encode(&Envelope {
            body: Some(Body::DnsQuery(control::DnsQuery {
                id,
                message: query_bytes(id as u16, text, TYPE_A),
                tcp: false,
            })),
        });
        guest.send(&frame).unwrap();
    }
    let answers = Mutex::new(Vec::new());
    for _ in 0..2 {
        let frame = guest.recv(Some(Duration::from_secs(5))).unwrap();
        let Ok(Envelope {
            body: Some(Body::DnsAnswer(answer)),
        }) = dsa_protocol::decode(&frame)
        else {
            panic!("the host sent something other than a DNS answer");
        };
        let message = wire::parse(&answer.message).unwrap();
        answers.lock().unwrap().push((answer.id, message.rcode));
    }
    let mut answers = answers.into_inner().unwrap();
    answers.sort_by_key(|(id, _)| *id);
    assert_eq!(answers, vec![(41, Rcode::NoError), (42, Rcode::NxDomain)]);
    stop.store(true, Ordering::SeqCst);
    host.join().unwrap();
}

/// A guest that only records the secret each boot was given.
#[derive(Clone, Default)]
struct SecretLauncher(Arc<Mutex<Option<dsa_protocol::auth::BootSecret>>>);

impl SecretLauncher {
    fn secret(&self) -> dsa_protocol::auth::BootSecret {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(secret) = self.0.lock().unwrap().clone() {
                return secret;
            }
            assert!(Instant::now() < deadline, "the guest never started");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl dsa_host_core::session::GuestLauncher for SecretLauncher {
    fn start(&mut self, secret: &dsa_protocol::auth::BootSecret) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(secret.clone());
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what} did not happen");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fr28_the_session_says_whether_the_hosts_resolver_answers_the_guest() {
    use dsa_host_core::handshake::ControlHandshake;
    use dsa_host_core::session::{Session, SessionConfig, SessionState};
    use dsa_protocol::control::Role;
    use dsa_protocol::handshake::{guest_connect, local_hello};

    let wait = Duration::from_secs(10);
    let launcher = SecretLauncher::default();
    let (listener, mut connector) = memory::pair();
    let config = SessionConfig {
        connect_timeout: wait,
        stop_timeout: wait,
        ..SessionConfig::default()
    };
    let session = Session::new(
        launcher.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host dns test", wait),
        config,
    );
    // A host that does not serve the DNS port cannot say who answers.
    assert_eq!(session.dns_answered_by_host(), None);
    let (dns_listener, mut dns_connector) = memory::pair();
    session.serve_dns_port(
        Box::new(dns_listener),
        Arc::new(FakeResolver::default()),
        "dsa-host dns test",
    );
    // Android is not running.
    assert_eq!(session.dns_answered_by_host(), None);

    session.start();
    let secret = launcher.secret();
    let mut agent = connector.connect(wait).expect("the agent connects");
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd dns test"),
        &secret,
        wait,
    )
    .expect("the host accepts the agent");
    agent
        .send(&dsa_protocol::encode(&Envelope {
            body: Some(Body::GuestBoot(control::GuestBoot {
                boot_completed: true,
                agent_connected: true,
            })),
        }))
        .expect("the agent sends");
    assert!(session.wait_for(SessionState::Active, wait));
    // Android runs but its control daemon has not reached the DNS port: it asks its network's
    // DNS servers.
    assert_eq!(session.dns_answered_by_host(), Some(false));

    let mut daemon = dns_connector.connect(wait).expect("the daemon connects");
    guest_connect(
        daemon.as_mut(),
        &local_hello(Role::Guest, "dsa-channeld dns test"),
        &secret,
        wait,
    )
    .expect("the host accepts the daemon");
    wait_until("the host counting the DNS connection", || {
        session.dns_answered_by_host() == Some(true)
    });

    // The connection ends (the host is away for the guest): its names go to the network again.
    daemon.close();
    drop(daemon);
    wait_until("the host noticing the DNS connection ended", || {
        session.dns_answered_by_host() == Some(false)
    });
    drop(agent);
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, wait));
    assert_eq!(session.dns_answered_by_host(), None);
}
