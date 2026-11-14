//! `dsa-channeld --keymint-key` against a host that runs the real handshake over the
//! in-memory transport, with a scratch folder standing in for the tmpfs folder init makes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_guest_channeld::keymint_key::{self, fetch, run, store, Retry, RootKey, FILE_NAME};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{envelope::Body, Envelope, KeyMintRootKey, Role};
use dsa_protocol::handshake::{host_accept, local_hello};
use dsa_transport::memory::{self, MemoryConnector, MemoryListener};
use dsa_transport::{Connection, Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

fn secret() -> BootSecret {
    BootSecret::from_bytes([0x5a; 32])
}

fn guest_hello() -> dsa_protocol::control::Hello {
    local_hello(Role::Guest, "dsa-channeld keymint test")
}

fn quick() -> Retry {
    Retry {
        give_up_after: Duration::from_secs(5),
        every: Duration::from_millis(5),
        attempt: Duration::from_secs(2),
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dsa-channeld-keymint-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The host's side of one connection: the handshake, the request, then `answer`.
fn host_answers(listener: &mut MemoryListener, answer: KeyMintRootKey) {
    let mut connection = listener.accept(WAIT).expect("the daemon connects");
    host_accept(
        connection.as_mut(),
        &local_hello(Role::Host, "dsa-hostd test"),
        &secret(),
        WAIT,
    )
    .expect("the handshake passes");
    let request = connection.recv(Some(WAIT)).expect("a request comes");
    assert!(matches!(
        dsa_protocol::decode(&request),
        Ok(Envelope {
            body: Some(Body::KeymintRootKeyRequest(_))
        })
    ));
    connection
        .send(&dsa_protocol::encode(&Envelope {
            body: Some(Body::KeymintRootKey(answer)),
        }))
        .unwrap();
}

/// Fails the first `failures` connections, as a host that does not listen yet would.
struct LateHost {
    inner: MemoryConnector,
    failures: Arc<AtomicU32>,
    attempts: Arc<AtomicU32>,
}

impl Connector for LateHost {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.failures.load(Ordering::SeqCst) > 0 {
            self.failures.fetch_sub(1, Ordering::SeqCst);
            return Err(TransportError::Io {
                kind: std::io::ErrorKind::ConnectionRefused,
                message: "nothing listens yet".into(),
            });
        }
        self.inner.connect(timeout)
    }
}

/// Never reaches a host.
struct NoHost(Arc<AtomicU32>);

impl Connector for NoHost {
    fn connect(&mut self, _timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(TransportError::Io {
            kind: std::io::ErrorKind::ConnectionRefused,
            message: "nothing listens".into(),
        })
    }
}

#[test]
fn fr36_keymint_key_comes_from_the_host_after_the_handshake() {
    let (mut listener, mut connector) = memory::pair();
    let host = thread::spawn(move || {
        host_answers(
            &mut listener,
            KeyMintRootKey {
                key: (0..32).collect(),
                message: String::new(),
            },
        )
    });
    let dir = scratch("key");
    let lines = Mutex::new(Vec::<String>::new());
    let said = |line: &str| lines.lock().unwrap().push(line.to_string());
    let code = run(
        &mut connector,
        &guest_hello(),
        Some(&secret()),
        &dir,
        &quick(),
        &said,
    );
    host.join().unwrap();
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
    assert_eq!(
        text,
        "key 000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\n"
    );
    let expected: Vec<u8> = (0..32).collect();
    assert_eq!(
        RootKey::parse(&text),
        Some(RootKey::Key(expected.try_into().unwrap()))
    );
    // Nothing the daemon says carries the key.
    assert!(lines.lock().unwrap().iter().all(|l| !l.contains("0a0b0c")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o400, "only KeyMint's user may read it");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr36_keymint_key_without_a_host_key_says_why() {
    let (mut listener, mut connector) = memory::pair();
    let host = thread::spawn(move || {
        host_answers(
            &mut listener,
            KeyMintRootKey {
                key: Vec::new(),
                message: "this computer keeps no key for Android's KeyMint\nsecond line".into(),
            },
        )
    });
    let dir = scratch("none");
    let code = run(
        &mut connector,
        &guest_hello(),
        Some(&secret()),
        &dir,
        &quick(),
        &|_| {},
    );
    host.join().unwrap();
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
    assert_eq!(
        text,
        "none this computer keeps no key for Android's KeyMint second line\n"
    );
    assert!(matches!(RootKey::parse(&text), Some(RootKey::None(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr36_keymint_key_waits_for_a_host_that_listens_late() {
    let (mut listener, connector) = memory::pair();
    let attempts = Arc::new(AtomicU32::new(0));
    let mut late = LateHost {
        inner: connector,
        failures: Arc::new(AtomicU32::new(3)),
        attempts: Arc::clone(&attempts),
    };
    let host = thread::spawn(move || {
        host_answers(
            &mut listener,
            KeyMintRootKey {
                key: vec![7; 32],
                message: String::new(),
            },
        )
    });
    let key = fetch(&mut late, &guest_hello(), &secret(), &quick(), &|_| {});
    host.join().unwrap();
    assert_eq!(key, RootKey::Key([7; 32]));
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
}

#[test]
fn fr36_keymint_key_gives_up_on_an_absent_host_and_writes_none() {
    let attempts = Arc::new(AtomicU32::new(0));
    let mut absent = NoHost(Arc::clone(&attempts));
    let retry = Retry {
        give_up_after: Duration::from_millis(200),
        every: Duration::from_millis(10),
        attempt: Duration::from_millis(50),
    };
    let dir = scratch("absent");
    let started = Instant::now();
    let code = run(
        &mut absent,
        &guest_hello(),
        Some(&secret()),
        &dir,
        &retry,
        &|_| {},
    );
    assert_eq!(code, 0);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it gives up in time"
    );
    assert!(
        attempts.load(Ordering::SeqCst) > 1,
        "it tried more than once"
    );
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
    assert!(
        text.starts_with("none the host could not be reached"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr36_keymint_key_of_the_wrong_length_is_not_used_or_asked_again() {
    let (mut listener, connector) = memory::pair();
    let attempts = Arc::new(AtomicU32::new(0));
    let mut counted = LateHost {
        inner: connector,
        failures: Arc::new(AtomicU32::new(0)),
        attempts: Arc::clone(&attempts),
    };
    let host = thread::spawn(move || {
        host_answers(
            &mut listener,
            KeyMintRootKey {
                key: vec![1; 16],
                message: String::new(),
            },
        )
    });
    let key = fetch(&mut counted, &guest_hello(), &secret(), &quick(), &|_| {});
    host.join().unwrap();
    match key {
        RootKey::None(why) => assert!(why.contains("16 bytes"), "{why}"),
        other => panic!("a 16-byte key was taken: {other:?}"),
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn fr36_keymint_key_is_not_given_to_a_guest_with_another_secret() {
    let (mut listener, mut connector) = memory::pair();
    let host = thread::spawn(move || {
        let mut connection = listener.accept(WAIT).expect("the daemon connects");
        let accepted = host_accept(
            connection.as_mut(),
            &local_hello(Role::Host, "dsa-hostd test"),
            &secret(),
            WAIT,
        );
        assert!(accepted.is_err(), "a wrong secret passes no handshake");
        connection.close();
    });
    let wrong = BootSecret::from_bytes([0x01; 32]);
    let retry = Retry {
        give_up_after: Duration::from_millis(500),
        every: Duration::from_millis(10),
        attempt: Duration::from_millis(200),
    };
    let key = fetch(&mut connector, &guest_hello(), &wrong, &retry, &|_| {});
    host.join().unwrap();
    assert!(matches!(key, RootKey::None(_)), "{key:?}");
}

#[test]
fn fr36_keymint_key_file_is_replaced_whole() {
    let dir = scratch("replace");
    store(&dir, &RootKey::None("first".into())).unwrap();
    store(&dir, &RootKey::Key([0xab; 32])).unwrap();
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
    assert_eq!(RootKey::parse(&text), Some(RootKey::Key([0xab; 32])));
    // Only the one file is left: nothing half written stays beside it.
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from(FILE_NAME)]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr36_keymint_key_file_text_is_read_strictly() {
    assert_eq!(RootKey::parse("key 00\n"), None);
    assert_eq!(RootKey::parse(&format!("key {}\n", "A".repeat(64))), None);
    assert_eq!(RootKey::parse(&format!("key {}", "a".repeat(64))), None);
    assert_eq!(RootKey::parse("garbage\n"), None);
    assert_eq!(
        RootKey::parse("none why\n"),
        Some(RootKey::None("why".into()))
    );
    assert_eq!(
        RootKey::None(String::new()).file_text(),
        "none the host gave no reason\n"
    );
    assert_eq!(keymint_key::PORT, 5604);
}
