//! The guest's side of the log stream, with logd and the log port faked: the in-memory
//! transport stands in for vsock, and a fake hands out logd's packets.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_guest_channeld::logbridge::{
    DiagnosticFiles, LogBridge, LogPackets, LogSource, PortConnector,
};
use dsa_guest_channeld::logd::{event_text, parse_entry, request, threadtime};
use dsa_guest_channeld::router::{Peer, Router, SendError};
use dsa_protocol::control::{
    envelope::Body, CollectDiagnostics, DiagnosticsDone, DiagnosticsFile, Envelope, LogDump,
    LogRecord, LogStream,
};
use dsa_protocol::{decode, encode};
use dsa_transport::memory::{self, MemoryListener};
use dsa_transport::{Connection, Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

/// One logd entry as logd sends it: a v4 header, then priority, tag and message.
fn packet(lid: u32, priority: u8, tag: &str, message: &str, sec: u32) -> Vec<u8> {
    let mut payload = vec![priority];
    payload.extend_from_slice(tag.as_bytes());
    payload.push(0);
    payload.extend_from_slice(message.as_bytes());
    payload.push(0);
    let mut out = Vec::new();
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(&28u16.to_le_bytes());
    out.extend_from_slice(&1234i32.to_le_bytes());
    out.extend_from_slice(&1240u32.to_le_bytes());
    out.extend_from_slice(&sec.to_le_bytes());
    out.extend_from_slice(&500_000_000u32.to_le_bytes());
    out.extend_from_slice(&lid.to_le_bytes());
    out.extend_from_slice(&1000u32.to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// logd, faked: each reader takes packets from one shared feed, and the fake counts the
/// readers open and the requests made.
#[derive(Clone)]
struct FakeLogd {
    requests: Arc<Mutex<Vec<String>>>,
    open: Arc<AtomicUsize>,
    consumed: Arc<AtomicUsize>,
    feed: Arc<Mutex<Receiver<Vec<u8>>>>,
}

struct FakePackets(FakeLogd);

impl Drop for FakePackets {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LogPackets for FakePackets {
    fn next(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        let feed = self.0.feed.lock().unwrap();
        match feed.recv_timeout(timeout) {
            Ok(packet) => {
                self.0.consumed.fetch_add(1, Ordering::SeqCst);
                Ok(Some(packet))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(Some(Vec::new())),
        }
    }
}

impl LogSource for FakeLogd {
    fn open(&self, request: &str) -> Result<Box<dyn LogPackets>, String> {
        self.requests.lock().unwrap().push(request.to_string());
        self.open.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakePackets(self.clone())))
    }
}

fn fake_logd() -> (FakeLogd, Sender<Vec<u8>>) {
    let (tx, rx) = mpsc::channel();
    (
        FakeLogd {
            requests: Arc::new(Mutex::new(Vec::new())),
            open: Arc::new(AtomicUsize::new(0)),
            consumed: Arc::new(AtomicUsize::new(0)),
            feed: Arc::new(Mutex::new(rx)),
        },
        tx,
    )
}

struct Pstore;

impl DiagnosticFiles for Pstore {
    fn files(&self) -> Vec<(String, Result<Vec<u8>, String>)> {
        vec![(
            "android/pstore/console-ramoops-0".into(),
            Ok(b"Kernel panic".to_vec()),
        )]
    }
}

/// The log port over the in-memory transport; `open` gates whether it can be reached.
fn port() -> (MemoryListener, Arc<PortConnector>, Arc<AtomicBool>) {
    let (listener, connector) = memory::pair();
    let connector = Mutex::new(connector);
    let open = Arc::new(AtomicBool::new(true));
    let gate = Arc::clone(&open);
    let connect: Arc<PortConnector> = Arc::new(move || {
        if !gate.load(Ordering::SeqCst) {
            return Err("the host is not listening".into());
        }
        connector
            .lock()
            .unwrap()
            .connect(WAIT)
            .map_err(|e| e.to_string())
    });
    (listener, connect, open)
}

fn bridge(logd: &FakeLogd, connect: Arc<PortConnector>, agent: bool) -> LogBridge {
    LogBridge::new(
        Arc::new(logd.clone()),
        Arc::new(Pstore),
        connect,
        Arc::new(move || agent),
    )
}

fn host(body: Body) -> Vec<u8> {
    encode(&Envelope { body: Some(body) })
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "{what} did not happen");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Every record that arrives on `connection` until `count` have.
fn records(connection: &mut Box<dyn Connection>, count: usize) -> Vec<LogRecord> {
    let mut got = Vec::new();
    let deadline = Instant::now() + WAIT;
    while got.len() < count {
        assert!(Instant::now() < deadline, "only {} records came", got.len());
        match connection.recv(Some(Duration::from_millis(50))) {
            Ok(frame) => match decode(&frame).unwrap().body {
                Some(Body::LogRecords(batch)) => got.extend(batch.records),
                other => panic!("expected records, got {other:?}"),
            },
            Err(TransportError::Timeout) => {}
            Err(error) => panic!("the log port ended: {error}"),
        }
    }
    got
}

#[test]
fn fr21_the_guest_reads_no_log_and_sends_nothing_until_the_host_asks() {
    let (logd, feed) = fake_logd();
    let (mut listener, connect, _) = port();
    let bridge = bridge(&logd, connect, true);
    assert!(bridge.from_host(&host(Body::LogStream(LogStream::default()))));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(logd.open.load(Ordering::SeqCst), 0, "logd was read");
    assert!(matches!(
        listener.accept(Duration::from_millis(200)),
        Err(TransportError::Timeout)
    ));

    assert!(bridge.from_host(&host(Body::LogStream(LogStream {
        on: true,
        buffers: 1 << 1,
        min_priority: 5,
        tail: 10,
    }))));
    let mut connection = listener.accept(WAIT).expect("the log port is opened");
    assert!(logd.requests.lock().unwrap()[0].starts_with("stream lids=0 tail=10"));
    feed.send(packet(0, 4, "Tag", "below the priority", 2)).unwrap();
    feed.send(packet(0, 6, "Tag", "wanted", 3)).unwrap();
    let got = records(&mut connection, 1);
    assert_eq!(got[0].message, "wanted");
    assert_eq!(got[0].buffer, 1);
    assert_eq!(got[0].priority, 6);
    assert_eq!(got[0].time_ns, 3_500_000_000);

    assert!(bridge.from_host(&host(Body::LogStream(LogStream::default()))));
    wait_until("logd's reader closing", || logd.open.load(Ordering::SeqCst) == 0);
    assert!(!bridge.streaming());
    feed.send(packet(0, 6, "Tag", "after the end", 4)).unwrap();
    thread::sleep(Duration::from_millis(300));
    if let Ok(frame) = connection.recv(Some(Duration::from_millis(200))) {
        panic!("sent {:?} after the stream ended", decode(&frame));
    }
}

#[test]
fn fr21_a_full_queue_drops_records_and_says_how_many_in_order() {
    let (logd, feed) = fake_logd();
    let (mut listener, connect, open) = port();
    open.store(false, Ordering::SeqCst);
    let bridge = bridge(&logd, connect, true).with_queue(4);
    bridge.from_host(&host(Body::LogStream(LogStream {
        on: true,
        buffers: 1 << 1,
        min_priority: 0,
        tail: 0,
    })));
    for i in 0..10 {
        feed.send(packet(0, 4, "T", &format!("r{i}"), i)).unwrap();
    }
    wait_until("logd being read", || logd.consumed.load(Ordering::SeqCst) == 10);
    open.store(true, Ordering::SeqCst);
    let mut connection = listener.accept(WAIT).expect("the log port is opened");
    let first = records(&mut connection, 4);
    let messages: Vec<_> = first.iter().map(|r| r.message.as_str()).collect();
    assert_eq!(messages, ["r0", "r1", "r2", "r3"]);
    feed.send(packet(0, 4, "T", "r10", 10)).unwrap();
    let rest = records(&mut connection, 2);
    assert_eq!(rest[0].dropped, 6);
    assert_eq!(rest[1].message, "r10");
}

#[test]
fn fr21_a_dump_sends_what_logd_holds_then_ends() {
    let (logd, feed) = fake_logd();
    let (mut listener, connect, _) = port();
    let bridge = bridge(&logd, connect, true);
    feed.send(packet(3, 6, "Sys", "kept", 1)).unwrap();
    feed.send(packet(3, 3, "Sys", "too low", 2)).unwrap();
    drop(feed);
    assert!(bridge.from_host(&host(Body::LogDump(LogDump {
        request_id: 5,
        buffers: 1 << 4,
        min_priority: 4,
    }))));
    let mut connection = listener.accept(WAIT).unwrap();
    let mut kept = Vec::new();
    loop {
        let frame = connection.recv(Some(WAIT)).unwrap();
        let Some(Body::LogRecords(batch)) = decode(&frame).unwrap().body else {
            panic!("expected records");
        };
        assert_eq!(batch.request_id, 5);
        kept.extend(batch.records);
        if batch.end {
            assert_eq!(batch.error, "");
            break;
        }
    }
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].message, "kept");
    assert!(logd.requests.lock().unwrap()[0].starts_with("dumpAndClose lids=3"));
}

#[test]
fn fr21_diagnostics_carry_the_log_pstore_and_the_agents_part() {
    let (logd, feed) = fake_logd();
    let (mut listener, connect, _) = port();
    let bridge = bridge(&logd, connect, true);
    let agent = bridge.agent_frames();
    feed.send(packet(0, 4, "Tag", "a line", 1)).unwrap();
    drop(feed);
    // The agent gathers its part too, so the frame goes on to it.
    assert!(!bridge.from_host(&host(Body::CollectDiagnostics(CollectDiagnostics {
        request_id: 9
    }))));
    let mut connection = listener.accept(WAIT).unwrap();
    agent
        .send(host(Body::DiagnosticsFile(DiagnosticsFile {
            request_id: 9,
            name: "android/dumpsys.txt".into(),
            data: b"ACTIVITY MANAGER".to_vec(),
            last: true,
        })))
        .unwrap();
    agent
        .send(host(Body::DiagnosticsDone(DiagnosticsDone {
            request_id: 9,
            error: "dropbox was refused".into(),
        })))
        .unwrap();
    let mut files = Vec::new();
    let done = loop {
        let frame = connection.recv(Some(WAIT)).unwrap();
        match decode(&frame).unwrap().body {
            Some(Body::DiagnosticsFile(file)) => {
                assert_eq!(file.request_id, 9);
                files.push((file.name, file.data));
            }
            Some(Body::DiagnosticsDone(done)) => break done,
            other => panic!("unexpected {other:?}"),
        }
    };
    let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "android/logcat.txt",
            "android/pstore/console-ramoops-0",
            "android/dumpsys.txt"
        ]
    );
    assert!(String::from_utf8_lossy(&files[0].1).contains("I Tag: a line"));
    assert_eq!(done.request_id, 9);
    assert!(done.error.contains("dropbox was refused"), "{}", done.error);
}

#[test]
fn fr21_logd_entries_become_records() {
    let record = parse_entry(&packet(4, 7, "crash_dump", "Fatal signal 11\n", 5)).unwrap();
    assert_eq!(record.buffer, 5);
    assert_eq!(record.priority, 7);
    assert_eq!(record.tag, "crash_dump");
    assert_eq!(record.message, "Fatal signal 11");
    assert_eq!(record.pid, 1234);
    assert_eq!(record.tid, 1240);
    assert_eq!(record.uid, 1000);
    assert!(threadtime(&record).ends_with(" 1234  1240 F crash_dump: Fatal signal 11\n"));

    // An event: tag 30000, then a list of an int and a string.
    let mut event = Vec::new();
    event.extend_from_slice(&30000u32.to_le_bytes());
    event.extend_from_slice(&[3, 2, 0]);
    event.extend_from_slice(&42i32.to_le_bytes());
    event.push(2);
    event.extend_from_slice(&3u32.to_le_bytes());
    event.extend_from_slice(b"abc");
    let mut packet = Vec::new();
    packet.extend_from_slice(&(event.len() as u16).to_le_bytes());
    packet.extend_from_slice(&28u16.to_le_bytes());
    packet.extend_from_slice(&[0; 24]);
    packet[20..24].copy_from_slice(&2u32.to_le_bytes());
    packet.extend_from_slice(&event);
    let record = parse_entry(&packet).unwrap();
    assert_eq!(record.buffer, 3);
    assert_eq!(record.tag, "30000");
    assert_eq!(record.message, "[42,abc]");
    assert_eq!(event_text(&[0, 1, 0, 0, 0]), "1");

    assert!(parse_entry(&[1, 2, 3]).is_err());
    let mut cut = long_packet();
    cut.truncate(30);
    assert!(parse_entry(&cut).is_err());

    assert_eq!(
        request((1 << 1) | (1 << 4), false, 0, Some(5_000_000_001)),
        "stream lids=0,3 start=5.000000001"
    );
    assert_eq!(request(1 << 8, true, 0, None), "dumpAndClose lids=7");
}

fn long_packet() -> Vec<u8> {
    packet(0, 4, "Tag", "a message long enough to be cut", 1)
}

#[test]
fn fr21_only_the_agent_sends_diagnostics_and_no_client_sends_the_log() {
    let (to_host, _host) = mpsc::channel();
    let agent = Peer(10);
    let router = Router::new(to_host, move |peer| {
        if peer == agent {
            Ok(())
        } else {
            Err("not the agent".into())
        }
    });
    let file = host(Body::DiagnosticsFile(DiagnosticsFile {
        request_id: 1,
        name: "android/dumpsys.txt".into(),
        data: Vec::new(),
        last: true,
    }));
    // Nothing is being gathered yet.
    assert!(matches!(
        router.from_guest(agent, file.clone()),
        Err(SendError::NotAllowed(_))
    ));
    let (diverted, from_router) = mpsc::channel();
    router.divert_diagnostics(diverted);
    assert!(matches!(
        router.from_guest(Peer(11), file.clone()),
        Err(SendError::NotAllowed(_))
    ));
    router.from_guest(agent, file.clone()).unwrap();
    assert_eq!(from_router.recv_timeout(WAIT).unwrap(), file);
    let records = host(Body::LogRecords(Default::default()));
    assert!(matches!(
        router.from_guest(agent, records),
        Err(SendError::NotAllowed(_))
    ));
}
