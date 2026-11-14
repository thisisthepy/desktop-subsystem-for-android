//! The root certificates a user or an administrator added to the host, trusted by Android as
//! system certificates: through the session's public API and the control channel, over the
//! in-memory transport, with a fake host trust store and a fake guest certificate installer
//! that keeps Android's own certificates and adds what the host sends, as the guest's control
//! daemon does.

use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_certificates::{
    self, added_roots, facts, source, FoundCertificate, HostRootStore,
};
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Role};
use dsa_protocol::encode;
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector};

const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------------------------
// Certificates, built by hand: enough of X.509 for the host to read the names and the validity.

fn der(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = contents.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend([0x81, len as u8]);
    } else {
        out.extend([0x82, (len >> 8) as u8, len as u8]);
    }
    out.extend_from_slice(contents);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

/// A Name of one common name and one organization.
fn name(common_name: &str, organization: &str) -> Vec<u8> {
    let attribute = |oid: u8, value: &str| {
        der(
            0x31,
            &seq(&[der(0x06, &[0x55, 0x04, oid]), der(0x0c, value.as_bytes())]),
        )
    };
    seq(&[attribute(0x0a, organization), attribute(0x03, common_name)])
}

/// A certificate for `subject`, signed (in name only) by `issuer`.
fn certificate(subject: &str, issuer: &str, serial: u8) -> Vec<u8> {
    let algorithm = seq(&[der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02])]);
    let tbs = seq(&[
        der(0xa0, &der(0x02, &[2])),
        der(0x02, &[serial]),
        algorithm.clone(),
        name(issuer, "Example"),
        seq(&[der(0x17, b"250101000000Z"), der(0x17, b"300101000000Z")]),
        name(subject, "Example"),
        seq(&[
            seq(&[
                der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01]),
                der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07]),
            ]),
            der(0x03, &[0, 4, serial, serial]),
        ]),
    ]);
    seq(&[tbs, algorithm, der(0x03, &[0, serial])])
}

fn root(name: &str, serial: u8) -> Vec<u8> {
    certificate(name, name, serial)
}

// ---------------------------------------------------------------------------------------------
// The fakes.

/// A guest that only records the secret each boot was given.
#[derive(Clone, Default)]
struct FakeLauncher(Arc<Mutex<Option<BootSecret>>>);

impl FakeLauncher {
    fn secret(&self) -> BootSecret {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(secret) = self.0.lock().unwrap().clone() {
                return secret;
            }
            assert!(Instant::now() < deadline, "the guest never started");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl GuestLauncher for FakeLauncher {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(secret.clone());
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

/// A host trust store in memory: what the stores hold, with the roots the operating system
/// ships among them, which the platform leaves out as every platform does, through
/// [`added_roots`].
#[derive(Default)]
struct FakeHostStore {
    state: Mutex<(Vec<FoundCertificate>, u64)>,
    changed: Condvar,
    built_in: Vec<Vec<u8>>,
}

impl FakeHostStore {
    fn with_built_in(built_in: Vec<Vec<u8>>) -> FakeHostStore {
        FakeHostStore {
            built_in,
            ..FakeHostStore::default()
        }
    }

    fn add(&self, der: Vec<u8>, source: &str) {
        let mut state = self.state.lock().unwrap();
        state
            .0
            .push(FoundCertificate::new(der, source, "fake store"));
        state.1 += 1;
        self.changed.notify_all();
    }

    fn remove(&self, der: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.0.retain(|found| found.der != der);
        state.1 += 1;
        self.changed.notify_all();
    }
}

impl HostRootStore for FakeHostStore {
    fn added_roots(&self) -> Result<Vec<FoundCertificate>, String> {
        let found = self.state.lock().unwrap().0.clone();
        Ok(added_roots(found, |found| {
            self.built_in.contains(&found.der)
        }))
    }

    fn wait_for_change(&self, timeout: Duration) {
        let state = self.state.lock().unwrap();
        let seen = state.1;
        let _ = self
            .changed
            .wait_timeout_while(state, timeout.min(WAIT), |state| state.1 == seen)
            .unwrap();
    }
}

/// The guest's certificate folders as the control daemon keeps them: Android's own, which
/// are never touched, and those the host sent, which are replaced by each message.
struct FakeGuestInstaller {
    android: BTreeSet<Vec<u8>>,
    added: BTreeSet<Vec<u8>>,
}

impl FakeGuestInstaller {
    fn new(android: &[Vec<u8>]) -> FakeGuestInstaller {
        FakeGuestInstaller {
            android: android.iter().cloned().collect(),
            added: BTreeSet::new(),
        }
    }

    fn install(&mut self, message: &control::HostCertificates) {
        self.added = if message.enabled {
            message
                .certificates
                .iter()
                .filter(|der| !self.android.contains(*der))
                .cloned()
                .collect()
        } else {
            BTreeSet::new()
        };
    }

    /// Whether an app reaches an HTTPS server whose certificate `server` chains to a root in
    /// the folders: the first certificate whose subject is the server's issuer.
    fn app_connects_to(&self, server: &[u8]) -> bool {
        let issuer = facts(server)
            .expect("the server's certificate reads")
            .issuer;
        self.android
            .iter()
            .chain(self.added.iter())
            .any(|root| facts(root).map(|f| f.subject == issuer).unwrap_or(false))
    }
}

// ---------------------------------------------------------------------------------------------
// The guest's side of the channel.

fn session_with(launcher: FakeLauncher) -> (Session, MemoryConnector) {
    let handshake = ControlHandshake::new("dsa-host test", WAIT);
    let (listener, connector) = memory::pair();
    let config = SessionConfig {
        connect_timeout: WAIT,
        stop_timeout: WAIT,
        ..SessionConfig::default()
    };
    (
        Session::new(launcher, Box::new(listener), handshake, config),
        connector,
    )
}

fn connect_guest(connector: &mut MemoryConnector, launcher: &FakeLauncher) -> Box<dyn Connection> {
    let secret = launcher.secret();
    let mut guest = connector.connect(WAIT).expect("the daemon connects");
    guest_connect(
        guest.as_mut(),
        &local_hello(Role::Guest, "dsa-channeld test"),
        &secret,
        WAIT,
    )
    .expect("the host accepts the daemon");
    guest
}

fn agent_connected(guest: &mut Box<dyn Connection>) {
    guest
        .send(&encode(&Envelope {
            body: Some(Body::GuestBoot(control::GuestBoot {
                boot_completed: true,
                agent_connected: true,
            })),
        }))
        .expect("the daemon sends");
}

/// The next HostCertificates the host sends, skipping the other messages.
fn next_certificates(guest: &mut Box<dyn Connection>) -> control::HostCertificates {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(
            Instant::now() < deadline,
            "the host never sent the certificates"
        );
        let frame = guest.recv(Some(WAIT)).expect("a message for the guest");
        let body = dsa_protocol::decode(&frame)
            .expect("well formed")
            .body
            .expect("a body");
        if let Body::HostCertificates(message) = body {
            return message;
        }
    }
}

/// Reads HostCertificates messages into `installer` until `done` holds of it.
fn install_until(
    guest: &mut Box<dyn Connection>,
    installer: &mut FakeGuestInstaller,
    done: impl Fn(&FakeGuestInstaller) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the guest never got there");
        installer.install(&next_certificates(guest));
        if done(installer) {
            return;
        }
    }
}

fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "{what} never happened");
        thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------------------------
// The requirement.

#[test]
fn fr28_android_trusts_the_root_certificates_the_host_added() {
    let android_root = root("Android Root", 1);
    let windows_root = root("Operating System Root", 2);
    let filter_root = root("Internet Filter Root", 3);
    let proxy_root = root("Company Proxy Root", 4);
    let filtered_server = certificate("www.example.com", "Internet Filter Root", 10);
    let proxied_server = certificate("intranet.example.com", "Company Proxy Root", 11);
    let shipped_server = certificate("update.example.com", "Operating System Root", 12);

    let store = Arc::new(FakeHostStore::with_built_in(vec![windows_root.clone()]));
    // The operating system's own root is in the store, as Windows keeps its own in the same
    // store a user's go to.
    store.add(windows_root.clone(), source::MACHINE);
    store.add(filter_root.clone(), source::MACHINE);

    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone());
    session.serve_host_certificates(Arc::clone(&store) as Arc<dyn HostRootStore>);
    wait_until("the store was read", || {
        session.host_certificates().len() == 1
    });
    let session = Arc::new(session);
    session.start();
    let mut guest = connect_guest(&mut connector, &launcher);
    let mut installer = FakeGuestInstaller::new(std::slice::from_ref(&android_root));

    // The daemon is told at once, before the agent is there: the filter's root, not the
    // operating system's.
    let first = next_certificates(&mut guest);
    assert!(first.enabled);
    assert_eq!(first.certificates, vec![filter_root.clone()]);
    installer.install(&first);
    assert!(installer.app_connects_to(&filtered_server));
    assert!(
        !installer.app_connects_to(&shipped_server),
        "the operating system's own roots are not given to Android"
    );
    agent_connected(&mut guest);
    assert!(session.wait_for(SessionState::Active, WAIT));

    // The settings app lists it, with where it was found.
    let listed = session.host_certificates();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].subject, "O=Example, CN=Internet Filter Root");
    assert_eq!(listed[0].source, source::MACHINE);
    assert_eq!(
        listed[0].sha256,
        host_certificates::sha256_hex(&filter_root)
    );
    assert_eq!(listed[0].not_after, 1_893_456_000);

    // A root added on the host while Android runs is trusted without a restart.
    assert!(!installer.app_connects_to(&proxied_server));
    store.add(proxy_root.clone(), source::USER_GROUP_POLICY);
    install_until(&mut guest, &mut installer, |guest| {
        guest.app_connects_to(&proxied_server)
    });
    assert!(installer.app_connects_to(&filtered_server));

    // One taken away on the host is no longer trusted.
    store.remove(&filter_root);
    install_until(&mut guest, &mut installer, |guest| {
        !guest.app_connects_to(&filtered_server)
    });
    assert!(installer.app_connects_to(&proxied_server));

    // Turned off, none is trusted, and Android's own list is as it was.
    session.trust_host_certificates(false);
    let off = next_certificates(&mut guest);
    assert!(!off.enabled && off.certificates.is_empty());
    installer.install(&off);
    assert!(!installer.app_connects_to(&proxied_server));
    assert_eq!(installer.android, BTreeSet::from([android_root.clone()]));
    assert_eq!(
        session.host_certificates().len(),
        1,
        "the settings app still lists what was found"
    );

    // Turned on again, they come back at once.
    session.trust_host_certificates(true);
    install_until(&mut guest, &mut installer, |guest| {
        guest.app_connects_to(&proxied_server)
    });
    session.stop();
}

#[test]
fn fr28_built_in_roots_repeats_and_unreadable_certificates_are_left_out() {
    let shipped = root("Shipped Root", 1);
    let added = root("Added Root", 2);
    let found = vec![
        FoundCertificate::new(shipped.clone(), source::MACHINE, "machine"),
        FoundCertificate::new(added.clone(), source::USER, "user"),
        FoundCertificate::new(added.clone(), source::MACHINE_GROUP_POLICY, "policy"),
        FoundCertificate::new(b"not a certificate".to_vec(), source::MACHINE, "machine"),
    ];
    let kept = added_roots(found, |found| found.der == shipped);
    assert_eq!(
        kept,
        vec![FoundCertificate::new(added, source::USER, "user")],
        "the first store a root was found in is the one named"
    );
}

#[test]
fn fr28_a_certificate_names_its_subject_issuer_and_end_of_validity() {
    let server = certificate("www.example.com", "Example Root", 7);
    let read = facts(&server).expect("it reads");
    assert_eq!(read.subject, "O=Example, CN=www.example.com");
    assert_eq!(read.issuer, "O=Example, CN=Example Root");
    assert_eq!(read.not_after, 1_893_456_000);
    assert_eq!(read.subject_der, name("www.example.com", "Example"));
    assert!(facts(&server[..server.len() - 1]).is_err(), "cut short");
}
