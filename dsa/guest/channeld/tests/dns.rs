//! The guest's DNS through the host: a program's query to the DNS stub goes over the DNS port
//! (the real handshake, the in-memory transport) to the host's DNS service, whose resolver is
//! a fake that knows a name only it has and blocks another. Also the stub's own answers when
//! the host is away, and the packet filter rules that send queries to the stub.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_guest_channeld::dns_redirect::{self, CHAIN, IP6TABLES, IPTABLES};
use dsa_guest_channeld::dns_stub::{self, serve_tcp, serve_udp, servfail, Tunnel, STUB_PORT};
use dsa_host_core::dns::wire::{self, Name, Rcode, Rdata, Record, TYPE_A};
use dsa_host_core::dns::{serve_connection, DnsService, HostResolver, Resolution};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::Role;
use dsa_protocol::handshake::{guest_connect, host_accept, local_hello};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connector, Listener};

const WAIT: Duration = Duration::from_secs(10);

/// The address only the host's resolver gives `HOST_ONLY`.
const HOST_ONLY: &str = "printer.corp.dsa.test";
const HOST_ONLY_ADDRESS: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);
/// A name the host's DNS filter blocks.
const BLOCKED: &str = "ads.blocked.dsa.test";

fn secret() -> BootSecret {
    BootSecret::from_bytes([0x3c; 32])
}

/// The host's resolver: it knows `HOST_ONLY` (from its hosts file, say) and blocks `BLOCKED`.
struct HostOnlyResolver;

impl HostResolver for HostOnlyResolver {
    fn resolve(&self, question: &wire::Question, _query: &[u8]) -> Resolution {
        let name = question.name.to_dotted().to_ascii_lowercase();
        if name == HOST_ONLY && question.qtype == TYPE_A {
            Resolution::answers(vec![Record::a(&question.name, 120, HOST_ONLY_ADDRESS)])
        } else if name == HOST_ONLY {
            Resolution::no_data()
        } else {
            Resolution::nxdomain()
        }
    }
}

/// The host's DNS port: accepts connections, passes the handshake, and serves each with the
/// DNS service, until `stop`.
fn host(mut listener: memory::MemoryListener, stop: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    let service = Arc::new(DnsService::new(Arc::new(HostOnlyResolver)));
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let Ok(mut connection) = listener.accept(Duration::from_millis(100)) else {
                continue;
            };
            host_accept(
                connection.as_mut(),
                &local_hello(Role::Host, "dsa-hostd dns test"),
                &secret(),
                WAIT,
            )
            .expect("the daemon passes the handshake");
            serve_connection(connection.as_mut(), &service, &stop);
        }
    })
}

/// The daemon's way to the host: the in-memory transport and the handshake.
fn connector(connector: MemoryConnector) -> Box<dns_stub::DnsConnector> {
    let connector = Mutex::new(connector);
    Box::new(move || {
        let mut connection = connector
            .lock()
            .unwrap()
            .connect(WAIT)
            .map_err(|e| e.to_string())?;
        guest_connect(
            connection.as_mut(),
            &local_hello(Role::Guest, "dsa-channeld dns test"),
            &secret(),
            WAIT,
        )
        .map_err(|e| e.to_string())?;
        Ok(connection)
    })
}

/// What the redirect was switched to, in order.
type Switches = Arc<Mutex<Vec<bool>>>;

fn recording(switches: &Switches) -> Box<dyn Fn(bool) + Send + Sync> {
    let switches = Arc::clone(switches);
    Box::new(move |on| switches.lock().unwrap().push(on))
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "{what} did not happen");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A program's UDP query to the stub at `stub`, and the answer it read.
fn ask_udp(stub: std::net::SocketAddr, id: u16, name: &str) -> wire::Message {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    let query = wire::encode(&wire::query(id, &Name::from_dotted(name).unwrap(), TYPE_A));
    socket.send_to(&query, stub).unwrap();
    let mut buffer = [0u8; 4096];
    let (len, _) = socket.recv_from(&mut buffer).expect("the stub answers");
    wire::parse(&buffer[..len]).unwrap()
}

/// The same over TCP, two length bytes and the message.
fn ask_tcp(stub: std::net::SocketAddr, id: u16, name: &str) -> wire::Message {
    let mut stream = TcpStream::connect(stub).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let query = wire::encode(&wire::query(id, &Name::from_dotted(name).unwrap(), TYPE_A));
    stream
        .write_all(&(query.len() as u16).to_be_bytes())
        .unwrap();
    stream.write_all(&query).unwrap();
    let mut len = [0u8; 2];
    stream.read_exact(&mut len).unwrap();
    let mut answer = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut answer).unwrap();
    wire::parse(&answer).unwrap()
}

#[test]
fn fr28_android_resolves_names_through_the_host() {
    let (listener, guest_side) = memory::pair();
    let stop = Arc::new(AtomicBool::new(false));
    let host = host(listener, Arc::clone(&stop));

    let switches: Switches = Arc::default();
    let tunnel = Arc::new(Tunnel::new(connector(guest_side), recording(&switches)));
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let (udp_at, tcp_at) = (udp.local_addr().unwrap(), tcp.local_addr().unwrap());
    let say: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|line: &str| eprintln!("{line}"));
    {
        let (tunnel, say) = (Arc::clone(&tunnel), Arc::clone(&say));
        thread::spawn(move || serve_udp(udp, tunnel, say.as_ref()));
    }
    {
        let (tunnel, say) = (Arc::clone(&tunnel), Arc::clone(&say));
        thread::spawn(move || serve_tcp(tcp, tunnel, say.as_ref()));
    }
    let runner = {
        let (tunnel, stop) = (Arc::clone(&tunnel), Arc::clone(&stop));
        thread::spawn(move || tunnel.run(&stop, &|line| eprintln!("{line}")))
    };
    wait_until("the stub reaching the host", || tunnel.connected());
    // The redirect goes on once the host answers, so the guest's queries reach the stub.
    assert_eq!(*switches.lock().unwrap(), vec![true]);

    // The name only the host's resolver knows resolves in the guest, to the same address.
    let answer = ask_udp(udp_at, 0x0101, HOST_ONLY);
    assert_eq!(answer.id, 0x0101);
    assert_eq!(answer.rcode, Rcode::NoError);
    assert_eq!(answer.answers.len(), 1);
    assert_eq!(
        answer.answers[0].data,
        Rdata::Raw(HOST_ONLY_ADDRESS.octets().to_vec())
    );
    assert_eq!(answer.answers[0].ttl, 120);
    let answer = ask_tcp(tcp_at, 0x0102, HOST_ONLY);
    assert_eq!(answer.id, 0x0102);
    assert_eq!(
        answer.answers[0].data,
        Rdata::Raw(HOST_ONLY_ADDRESS.octets().to_vec())
    );

    // The name the host blocks does not resolve in the guest either.
    let blocked = ask_udp(udp_at, 0x0201, BLOCKED);
    assert_eq!(blocked.rcode, Rcode::NxDomain);
    assert!(blocked.answers.is_empty());

    stop.store(true, Ordering::SeqCst);
    runner.join().unwrap();
    host.join().unwrap();
    // Stopping switches the redirect off again.
    assert_eq!(*switches.lock().unwrap(), vec![true, false]);
}

#[test]
fn fr28_while_the_host_is_away_queries_get_servfail_at_once_and_the_redirect_goes_off() {
    let switches: Switches = Arc::default();
    let tunnel = Arc::new(
        Tunnel::new(
            Box::new(|| Err("the host does not listen".to_string())),
            recording(&switches),
        )
        .with_timeouts(Duration::from_secs(5), Duration::from_millis(200)),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let runner = {
        let (tunnel, stop) = (Arc::clone(&tunnel), Arc::clone(&stop));
        thread::spawn(move || tunnel.run(&stop, &|line| eprintln!("{line}")))
    };
    let query = wire::encode(&wire::query(
        7,
        &Name::from_dotted(HOST_ONLY).unwrap(),
        TYPE_A,
    ));
    let started = Instant::now();
    let answer = dns_stub::answer(&tunnel, &query, false).unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    let answer = wire::parse(&answer).unwrap();
    assert_eq!(answer.id, 7);
    assert_eq!(answer.rcode, Rcode::ServFail);
    // The redirect is switched off, so Android asks its network's DNS servers meanwhile.
    wait_until("the redirect going off", || {
        switches.lock().unwrap().as_slice() == [false]
    });
    stop.store(true, Ordering::SeqCst);
    runner.join().unwrap();
}

#[test]
fn fr28_a_host_that_does_not_answer_in_time_gets_the_program_servfail() {
    // A host that passes the handshake and then reads the query without answering.
    let (mut listener, guest_side) = memory::pair();
    let host = thread::spawn(move || {
        let mut connection = listener.accept(WAIT).unwrap();
        host_accept(
            connection.as_mut(),
            &local_hello(Role::Host, "dsa-hostd silent"),
            &secret(),
            WAIT,
        )
        .unwrap();
        let _query = connection.recv(Some(WAIT));
        thread::sleep(Duration::from_secs(1));
    });
    let switches: Switches = Arc::default();
    let tunnel = Arc::new(
        Tunnel::new(connector(guest_side), recording(&switches))
            .with_timeouts(Duration::from_millis(200), Duration::from_secs(30)),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let runner = {
        let (tunnel, stop) = (Arc::clone(&tunnel), Arc::clone(&stop));
        thread::spawn(move || tunnel.run(&stop, &|line| eprintln!("{line}")))
    };
    wait_until("the stub reaching the host", || tunnel.connected());
    let query = wire::encode(&wire::query(
        9,
        &Name::from_dotted(HOST_ONLY).unwrap(),
        TYPE_A,
    ));
    let started = Instant::now();
    let answer = wire::parse(&dns_stub::answer(&tunnel, &query, false).unwrap()).unwrap();
    assert_eq!(answer.rcode, Rcode::ServFail);
    assert!(started.elapsed() < Duration::from_secs(2));
    stop.store(true, Ordering::SeqCst);
    runner.join().unwrap();
    host.join().unwrap();
}

#[test]
fn fr28_the_stubs_servfail_keeps_the_querys_id_and_question() {
    let mut query = wire::query(0xabcd, &Name::from_dotted("x.dsa.test").unwrap(), TYPE_A);
    query.flags |= 0x0010; // CD
    let bytes = wire::encode(&query);
    let answer = wire::parse(&servfail(&bytes).unwrap()).unwrap();
    assert_eq!(answer.id, 0xabcd);
    assert!(answer.is_response());
    assert!(answer.recursion_desired());
    assert!(answer.checking_disabled());
    assert_eq!(answer.rcode, Rcode::ServFail);
    assert_eq!(answer.questions, query.questions);
    // A cut question still gets its SERVFAIL, without the question.
    let answer = wire::parse(&servfail(&bytes[..15]).unwrap()).unwrap();
    assert_eq!(answer.id, 0xabcd);
    assert!(answer.questions.is_empty());
    // Too short for a header, or itself an answer: nothing to answer.
    assert!(servfail(&bytes[..11]).is_none());
    let mut response = bytes.clone();
    response[2] |= 0x80;
    assert!(servfail(&response).is_none());
}

#[test]
fn fr28_the_guest_and_the_host_agree_on_the_dns_port() {
    assert_eq!(dns_stub::PORT, dsa_host_core::dns::DNS_PORT);
    assert_ne!(STUB_PORT, 53);
    assert_ne!(STUB_PORT, 5353);
}

#[test]
fn fr28_the_redirect_sends_udp_and_tcp_port_53_to_the_stub_once() {
    let steps = dns_redirect::steps(true, "127.0.0.0/8");
    let lines: Vec<String> = steps.iter().map(|s| s.args.join(" ")).collect();
    let port = STUB_PORT.to_string();
    for protocol in ["udp", "tcp"] {
        let rule = format!(
            "-w -t nat -A {CHAIN} ! -d 127.0.0.0/8 -p {protocol} --dport 53 -j REDIRECT --to-ports {port}"
        );
        assert!(lines.contains(&rule), "{lines:?}");
    }
    // The chain is emptied before its rules go in, and the jump is removed before it is
    // inserted, so running it twice leaves one of each.
    let flush = lines
        .iter()
        .position(|l| l.contains(&format!("-F {CHAIN}")))
        .unwrap();
    let first_rule = lines.iter().position(|l| l.contains("REDIRECT")).unwrap();
    assert!(flush < first_rule);
    let delete = lines.iter().position(|l| l.contains("-D OUTPUT")).unwrap();
    let insert = lines.iter().position(|l| l.contains("-I OUTPUT")).unwrap();
    assert!(delete < insert);
    assert!(steps[delete].may_fail && !steps[insert].may_fail);
    // Off empties the chain and adds nothing.
    let off: Vec<String> = dns_redirect::steps(false, "::1/128")
        .iter()
        .map(|s| s.args.join(" "))
        .collect();
    assert!(off
        .iter()
        .all(|l| !l.contains("REDIRECT") && !l.contains("-I OUTPUT")));
    assert!(off.iter().any(|l| l.contains(&format!("-F {CHAIN}"))));
}

#[test]
fn fr28_the_redirect_follows_the_property_and_survives_a_family_without_nat() {
    let property = Mutex::new(vec!["on".to_string(), "on".to_string()]);
    let ran: Mutex<Vec<String>> = Mutex::default();
    let run = |program: &str, args: &[String]| -> Result<(), String> {
        ran.lock()
            .unwrap()
            .push(format!("{program} {}", args.join(" ")));
        if program == IP6TABLES {
            Err("can't initialize ip6tables table `nat'".into())
        } else {
            Ok(())
        }
    };
    let read = |_name: &str| {
        let mut values = property.lock().unwrap();
        if values.len() > 1 {
            values.remove(0)
        } else {
            values[0].clone()
        }
    };
    let lines: Mutex<Vec<String>> = Mutex::default();
    let code = dns_redirect::run(&read, &run, &|line| lines.lock().unwrap().push(line.into()));
    assert_eq!(code, 0);
    let ran = ran.into_inner().unwrap();
    assert!(ran
        .iter()
        .any(|l| l.starts_with(IPTABLES) && l.contains("REDIRECT")));
    let lines = lines.into_inner().unwrap();
    assert!(lines.iter().any(|l| l.contains(IP6TABLES)), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("go to the DNS stub")),
        "{lines:?}"
    );

    // A property that changes while the rules are made is followed to its last value.
    let property = Mutex::new(vec!["on".to_string(), "off".to_string()]);
    let switched: Mutex<Vec<bool>> = Mutex::default();
    let read = |_name: &str| {
        let mut values = property.lock().unwrap();
        if values.len() > 1 {
            values.remove(0)
        } else {
            values[0].clone()
        }
    };
    let run = |_program: &str, args: &[String]| -> Result<(), String> {
        if args.iter().any(|a| a == "-F") {
            switched.lock().unwrap().push(false);
        }
        if args.iter().any(|a| a == "REDIRECT") {
            *switched.lock().unwrap().last_mut().unwrap() = true;
        }
        Ok(())
    };
    dns_redirect::run(&read, &run, &|_| {});
    // Each family once on, then each once off.
    assert_eq!(
        switched.into_inner().unwrap(),
        vec![true, true, false, false]
    );
}

#[test]
fn fr28_the_redirect_needs_the_word_on() {
    assert!(dns_redirect::wanted("on"));
    assert!(dns_redirect::wanted(" on\n"));
    assert!(!dns_redirect::wanted("off"));
    assert!(!dns_redirect::wanted(""));
    assert!(!dns_redirect::wanted("1"));
}
