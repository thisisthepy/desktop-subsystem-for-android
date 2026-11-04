//! The guest's log stream and the diagnostics, through the session's public API and the
//! local API, over the in-memory transport.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use dsa_host_core::diagnostics::{crc32, lines_of_user, write_archive, HostFile, ZipWriter};
use dsa_host_core::guest_log::{
    read_log_frame, DiagnosticsEvent, LogBuffer, LogDumpEvent, LogEntry, LogFilter, LogHub,
    LogPortMessage, LogRing, DEFAULT_BUFFERS, ERROR, INFO, SILENT, VERBOSE, WARN,
};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Role};
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_protocol::{decode, encode};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector, TransportError};

const WAIT: Duration = Duration::from_secs(10);

fn entry(buffer: LogBuffer, priority: u8, tag: &str, message: &str) -> LogEntry {
    LogEntry {
        buffer,
        priority,
        time_ns: 1_700_000_000_000_000_000,
        pid: 100,
        tid: 101,
        uid: 1000,
        tag: tag.into(),
        message: message.into(),
        dropped: 0,
    }
}

#[test]
fn fr21_the_host_ring_stays_within_its_size_and_drops_the_oldest_first() {
    let one = entry(LogBuffer::Main, INFO, "tag", "0123456789");
    let mut ring = LogRing::new(one.size() * 3);
    for i in 0..5 {
        ring.push(entry(LogBuffer::Main, INFO, "tag", &format!("{i:010}")));
        assert!(ring.bytes() <= one.size() * 3);
    }
    let kept: Vec<_> = ring.iter().map(|e| e.message.clone()).collect();
    assert_eq!(kept, ["0000000002", "0000000003", "0000000004"]);
    // A record larger than the whole ring is not kept, and nothing else is lost for it.
    ring.push(entry(LogBuffer::Main, INFO, "tag", &"x".repeat(one.size() * 4)));
    assert_eq!(ring.len(), 3);
}

#[test]
fn fr21_filters_pick_records_by_buffer_priority_and_tag() {
    let filter = LogFilter {
        buffers: LogFilter::parse_buffers("main,crash").unwrap(),
        tags: vec![
            LogFilter::parse_spec("ActivityManager:I").unwrap(),
            LogFilter::parse_spec("chatty:S").unwrap(),
            LogFilter::parse_spec("*:W").unwrap(),
        ],
    };
    assert!(filter.passes(&entry(LogBuffer::Main, INFO, "ActivityManager", "start")));
    assert!(!filter.passes(&entry(LogBuffer::Main, 3, "ActivityManager", "debug")));
    assert!(!filter.passes(&entry(LogBuffer::Main, ERROR, "chatty", "silenced")));
    assert!(!filter.passes(&entry(LogBuffer::Main, INFO, "Other", "below W")));
    assert!(filter.passes(&entry(LogBuffer::Crash, WARN, "Other", "W")));
    assert!(!filter.passes(&entry(LogBuffer::System, ERROR, "Other", "not a wanted buffer")));
    // A record that stands for dropped ones always passes, so the reader learns of the gap.
    assert!(filter.passes(&LogEntry::dropped(3)));
    assert_eq!(filter.least_priority(), INFO);

    assert_eq!(LogFilter::default().buffers, DEFAULT_BUFFERS);
    assert_eq!(LogFilter::parse_spec("Tag").unwrap(), ("Tag".into(), VERBOSE));
    assert_eq!(LogFilter::parse_spec("*:S").unwrap(), ("*".into(), SILENT));
    assert!(LogFilter::parse_spec("Tag:Q").is_err());
    assert!(LogFilter::parse_spec(":I").is_err());
    assert!(LogFilter::parse_buffers("main,nope").is_err());

    // The filter survives the local API.
    let wire = filter.to_host_api(false);
    assert_eq!(LogFilter::from_host_api(&wire).unwrap(), filter);
}

#[test]
fn fr21_without_a_reader_the_guest_is_told_to_send_nothing() {
    let mut hub = LogHub::default();
    assert!(!hub.stream().on);
    let (first, _, _first_rx) = hub.subscribe(LogFilter {
        buffers: LogBuffer::Main.bit(),
        tags: vec![("*".into(), WARN)],
    });
    let (second, _, _second_rx) = hub.subscribe(LogFilter {
        buffers: LogBuffer::Crash.bit(),
        tags: vec![("*".into(), ERROR), ("Tag".into(), INFO)],
    });
    let stream = hub.stream();
    assert!(stream.on);
    assert_eq!(stream.buffers, LogBuffer::Main.bit() | LogBuffer::Crash.bit());
    assert_eq!(stream.min_priority, u32::from(INFO));
    hub.unsubscribe(first);
    hub.unsubscribe(second);
    assert_eq!(hub.stream(), control::LogStream::default());
}

#[test]
fn fr21_a_reader_that_falls_behind_loses_records_and_is_told_how_many_in_order() {
    let mut hub = LogHub::new(1 << 20, 2);
    let (_, _, rx) = hub.subscribe(LogFilter::default());
    let records: Vec<_> = (0..5)
        .map(|i| entry(LogBuffer::Main, INFO, "t", &format!("{i}")))
        .collect();
    hub.publish(records);
    assert_eq!(rx.try_recv().unwrap().message, "0");
    assert_eq!(rx.try_recv().unwrap().message, "1");
    assert!(rx.try_recv().is_err());
    hub.publish(vec![entry(LogBuffer::Main, INFO, "t", "5")]);
    let marker = rx.try_recv().unwrap();
    assert_eq!(marker.dropped, 3);
    assert_eq!(rx.try_recv().unwrap().message, "5");
    // The ring kept all of them for the next reader.
    assert_eq!(hub.ring().len(), 6);
}

#[test]
fn fr21_a_malformed_record_is_an_error_never_a_panic() {
    let bad_buffer = encode(&Envelope {
        body: Some(Body::LogRecords(control::LogRecords {
            records: vec![control::LogRecord {
                buffer: 99,
                priority: 4,
                ..Default::default()
            }],
            ..Default::default()
        })),
    });
    assert!(read_log_frame(&bad_buffer).is_err());
    let bad_priority = encode(&Envelope {
        body: Some(Body::LogRecords(control::LogRecords {
            records: vec![control::LogRecord {
                buffer: 1,
                priority: 200,
                ..Default::default()
            }],
            ..Default::default()
        })),
    });
    assert!(read_log_frame(&bad_priority).is_err());
    assert!(read_log_frame(b"\xff\xff\xff").is_err());
    let control_message = encode(&Envelope {
        body: Some(Body::ScreenState(control::ScreenState { on: true })),
    });
    assert!(read_log_frame(&control_message).is_err());
    for name in ["../escape", "android/../../x", "/abs", "android/a\\b", "android/"] {
        let file = encode(&Envelope {
            body: Some(Body::DiagnosticsFile(control::DiagnosticsFile {
                request_id: 1,
                name: name.into(),
                data: Vec::new(),
                last: true,
            })),
        });
        assert!(read_log_frame(&file).is_err(), "{name} was taken");
    }
    let good = encode(&Envelope {
        body: Some(Body::LogRecords(control::LogRecords {
            records: vec![entry(LogBuffer::Main, INFO, "t", "m").to_control()],
            ..Default::default()
        })),
    });
    assert!(matches!(
        read_log_frame(&good),
        Ok(LogPortMessage::Records { request_id: 0, .. })
    ));
}

#[test]
fn fr21_records_print_as_logcat_threadtime() {
    let line = entry(LogBuffer::Main, WARN, "Tag", "first\nsecond").threadtime();
    assert_eq!(
        line,
        "2023-11-14 22:13:20.000   100   101 W Tag: first\n\
         2023-11-14 22:13:20.000   100   101 W Tag: second"
    );
    assert_eq!(LogEntry::dropped(4).threadtime(), "--------- 4 records dropped");
}

/// Reads one zip archive's stored files.
fn unzip(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
    let end = archive.len() - 22;
    assert_eq!(&archive[end..end + 4], &0x0605_4b50u32.to_le_bytes());
    let count = u16::from_le_bytes([archive[end + 10], archive[end + 11]]) as usize;
    let mut at = u32::from_le_bytes(archive[end + 16..end + 20].try_into().unwrap()) as usize;
    let mut files = Vec::new();
    for _ in 0..count {
        assert_eq!(&archive[at..at + 4], &0x0201_4b50u32.to_le_bytes());
        let crc = u32::from_le_bytes(archive[at + 16..at + 20].try_into().unwrap());
        let size = u32::from_le_bytes(archive[at + 20..at + 24].try_into().unwrap()) as usize;
        let name_len = u16::from_le_bytes([archive[at + 28], archive[at + 29]]) as usize;
        let offset = u32::from_le_bytes(archive[at + 42..at + 46].try_into().unwrap()) as usize;
        let name = String::from_utf8(archive[at + 46..at + 46 + name_len].to_vec()).unwrap();
        let local_name_len =
            u16::from_le_bytes([archive[offset + 26], archive[offset + 27]]) as usize;
        let data_at = offset + 30 + local_name_len;
        let data = archive[data_at..data_at + size].to_vec();
        assert_eq!(crc32(&data), crc, "{name}'s checksum");
        files.push((name, data));
        at += 46 + name_len;
    }
    files
}

#[test]
fn fr21_the_archive_is_a_zip_of_the_guest_and_host_files() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    let folder = std::env::temp_dir().join(format!("dsa-diagnostics-test-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let host_log = folder.join("dsa-hostd.log");
    std::fs::write(&host_log, b"service started\n").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(DiagnosticsEvent::File {
        name: "android/logcat.txt".into(),
        data: b"one ".to_vec(),
        last: false,
    })
    .unwrap();
    tx.send(DiagnosticsEvent::File {
        name: "android/logcat.txt".into(),
        data: b"two".to_vec(),
        last: true,
    })
    .unwrap();
    tx.send(DiagnosticsEvent::Done(Some("dumpsys was refused".into())))
        .unwrap();
    let (archive, report) = write_archive(
        Vec::new(),
        "test archive",
        Some(rx),
        None,
        vec![("host/android-recent.log".into(), b"recent".to_vec())],
        &[
            HostFile {
                name: "host/dsa-hostd.log".into(),
                path: host_log,
                only_user: None,
            },
            HostFile {
                name: "host/missing.log".into(),
                path: folder.join("missing.log"),
                only_user: None,
            },
        ],
        SystemTime::now(),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&folder);
    let files = unzip(&archive);
    let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "android/logcat.txt",
            "host/android-recent.log",
            "host/dsa-hostd.log",
            "summary.txt"
        ]
    );
    assert_eq!(files[0].1, b"one two");
    let summary = String::from_utf8(files[3].1.clone()).unwrap();
    assert!(summary.contains("dumpsys was refused"), "{summary}");
    assert_eq!(report.problems.len(), 1);

    // An archive with nothing in it is still a valid one.
    let empty = ZipWriter::new(Vec::new(), SystemTime::now()).finish().unwrap();
    assert!(unzip(&empty).is_empty());
}

/// A guest whose only state is the secret its daemon would know.
#[derive(Clone, Default)]
struct Launcher(Arc<Mutex<Option<BootSecret>>>);

impl Launcher {
    fn secret(&self) -> BootSecret {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(secret) = self.0.lock().unwrap().clone() {
                return secret;
            }
            assert!(Instant::now() < deadline, "the guest was never started");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl GuestLauncher for Launcher {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(secret.clone());
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

struct Guest {
    session: Session,
    launcher: Launcher,
    control: Box<dyn Connection>,
    log_port: MemoryConnector,
}

fn running_guest() -> Guest {
    let (listener, mut connector) = memory::pair();
    let (log_listener, log_port) = memory::pair();
    let launcher = Launcher::default();
    let session = Session::new(
        launcher.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            ..SessionConfig::default()
        },
    );
    session.serve_log_port(Box::new(log_listener), "dsa-host test");
    session.start();
    let secret = launcher.secret();
    let mut control = connector.connect(WAIT).unwrap();
    guest_connect(
        control.as_mut(),
        &local_hello(Role::Guest, "dsa-channeld test"),
        &secret,
        WAIT,
    )
    .unwrap();
    control
        .send(&encode(&Envelope {
            body: Some(Body::GuestBoot(control::GuestBoot {
                boot_completed: true,
                agent_connected: true,
            })),
        }))
        .unwrap();
    assert!(session.wait_for(SessionState::Active, WAIT));
    Guest {
        session,
        launcher,
        control,
        log_port,
    }
}

impl Guest {
    /// The next message of a kind `pick` takes, skipping the others the host sends.
    fn next<T>(&mut self, pick: impl Fn(Body) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the host never sent what was expected");
            match self.control.recv(Some(left)) {
                Ok(frame) => {
                    if let Some(body) = decode(&frame).unwrap().body {
                        if let Some(found) = pick(body) {
                            return found;
                        }
                    }
                }
                Err(TransportError::Timeout) => {}
                Err(error) => panic!("the control channel ended: {error}"),
            }
        }
    }

    fn next_stream(&mut self) -> control::LogStream {
        self.next(|body| match body {
            Body::LogStream(stream) => Some(stream),
            _ => None,
        })
    }

    fn connect_log(&mut self) -> Box<dyn Connection> {
        let secret = self.launcher.secret();
        let mut log = self.log_port.connect(WAIT).unwrap();
        guest_connect(
            log.as_mut(),
            &local_hello(Role::Guest, "dsa-channeld test"),
            &secret,
            WAIT,
        )
        .unwrap();
        log
    }
}

fn send_records(log: &mut Box<dyn Connection>, request_id: u32, records: &[LogEntry], end: bool) {
    log.send(&encode(&Envelope {
        body: Some(Body::LogRecords(control::LogRecords {
            request_id,
            records: records.iter().map(LogEntry::to_control).collect(),
            end,
            error: String::new(),
        })),
    }))
    .unwrap();
}

#[test]
fn fr21_the_guest_streams_only_while_a_reader_is_attached() {
    let mut guest = running_guest();
    let watch = guest.session.watch_log(LogFilter {
        buffers: LogBuffer::Main.bit(),
        tags: vec![("*".into(), WARN)],
    });
    let stream = guest.next_stream();
    assert!(stream.on);
    assert_eq!(stream.buffers, LogBuffer::Main.bit());
    assert_eq!(stream.min_priority, u32::from(WARN));

    let mut log = guest.connect_log();
    send_records(
        &mut log,
        0,
        &[
            entry(LogBuffer::Main, ERROR, "Tag", "wanted"),
            entry(LogBuffer::Main, INFO, "Tag", "below the filter"),
        ],
        false,
    );
    let got = watch.entries.recv_timeout(WAIT).unwrap();
    assert_eq!(got.message, "wanted");
    assert!(watch.entries.recv_timeout(Duration::from_millis(200)).is_err());

    drop(watch);
    assert!(!guest.next_stream().on);
}

#[test]
fn fr21_a_malformed_record_closes_only_the_log_connection() {
    let mut guest = running_guest();
    let watch = guest.session.watch_log(LogFilter::default());
    assert!(guest.next_stream().on);
    let mut log = guest.connect_log();
    log.send(&encode(&Envelope {
        body: Some(Body::LogRecords(control::LogRecords {
            records: vec![control::LogRecord {
                buffer: 42,
                priority: 4,
                ..Default::default()
            }],
            ..Default::default()
        })),
    }))
    .unwrap();
    // The host closes the log connection...
    let deadline = Instant::now() + WAIT;
    loop {
        match log.recv(Some(Duration::from_millis(50))) {
            Err(TransportError::Timeout) => {
                assert!(Instant::now() < deadline, "the log connection stayed open");
            }
            Err(_) => break,
            Ok(_) => {}
        }
    }
    // ...and the session, its control channel and a new log connection go on.
    assert_eq!(guest.session.state(), SessionState::Active);
    guest
        .control
        .send(&encode(&Envelope {
            body: Some(Body::DeviceIdle(control::DeviceIdle { idle: false })),
        }))
        .unwrap();
    let mut again = guest.connect_log();
    send_records(&mut again, 0, &[entry(LogBuffer::Main, INFO, "T", "after")], false);
    assert_eq!(watch.entries.recv_timeout(WAIT).unwrap().message, "after");
}

#[test]
fn fr21_a_connection_without_the_boot_secret_is_refused_on_the_log_port() {
    let mut guest = running_guest();
    let mut log = guest.log_port.connect(WAIT).unwrap();
    let wrong = BootSecret::generate().unwrap();
    assert!(guest_connect(
        log.as_mut(),
        &local_hello(Role::Guest, "an app"),
        &wrong,
        WAIT
    )
    .is_err());
    // The real daemon still gets in.
    let _ = guest.connect_log();
}

#[test]
fn fr21_a_dump_sends_what_the_guest_holds_then_ends() {
    let mut guest = running_guest();
    let events = guest
        .session
        .dump_log(LogFilter {
            buffers: DEFAULT_BUFFERS,
            tags: vec![("*".into(), WARN)],
        })
        .unwrap();
    let dump = guest.next(|body| match body {
        Body::LogDump(dump) => Some(dump),
        _ => None,
    });
    assert_eq!(dump.min_priority, u32::from(WARN));
    let mut log = guest.connect_log();
    send_records(
        &mut log,
        dump.request_id,
        &[
            entry(LogBuffer::Main, ERROR, "T", "kept"),
            entry(LogBuffer::Main, INFO, "T", "filtered"),
        ],
        false,
    );
    send_records(&mut log, dump.request_id, &[], true);
    assert_eq!(
        events.recv_timeout(WAIT).unwrap(),
        LogDumpEvent::Entries(vec![entry(LogBuffer::Main, ERROR, "T", "kept")])
    );
    assert_eq!(events.recv_timeout(WAIT).unwrap(), LogDumpEvent::End(None));
}

#[test]
fn fr21_the_guest_diagnostics_arrive_on_the_log_port() {
    let mut guest = running_guest();
    let events = guest.session.collect_diagnostics().unwrap();
    let request = guest.next(|body| match body {
        Body::CollectDiagnostics(request) => Some(request),
        _ => None,
    });
    let mut log = guest.connect_log();
    log.send(&encode(&Envelope {
        body: Some(Body::DiagnosticsFile(control::DiagnosticsFile {
            request_id: request.request_id,
            name: "android/pstore/console-ramoops-0".into(),
            data: b"panic".to_vec(),
            last: true,
        })),
    }))
    .unwrap();
    log.send(&encode(&Envelope {
        body: Some(Body::DiagnosticsDone(control::DiagnosticsDone {
            request_id: request.request_id,
            error: String::new(),
        })),
    }))
    .unwrap();
    assert_eq!(
        events.recv_timeout(WAIT).unwrap(),
        DiagnosticsEvent::File {
            name: "android/pstore/console-ramoops-0".into(),
            data: b"panic".to_vec(),
            last: true
        }
    );
    assert_eq!(events.recv_timeout(WAIT).unwrap(), DiagnosticsEvent::Done(None));
}

#[test]
fn fr21_a_users_diagnostics_leave_out_the_service_lines_about_other_users() {
    let log = b"service started
S-1-5-21-1-1001: lifecycle: Android finished booting
S-1-5-21-1-1002: lifecycle: the agent is connected
local API: refused a client
";
    let kept = lines_of_user(log, "S-1-5-21-1-1001");
    assert_eq!(
        String::from_utf8(kept).unwrap(),
        "service started
S-1-5-21-1-1001: lifecycle: Android finished booting
local API: refused a client
"
    );
}
