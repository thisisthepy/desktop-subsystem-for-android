//! The connection handshake over the in-memory transport.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dsa_protocol::auth::{hmac_sha256, BootSecret};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{AuthProof, Envelope, RejectReason, Role};
use dsa_protocol::handshake::{guest_connect, host_accept, local_hello, HandshakeError};
use dsa_protocol::{encode, PROTOCOL_VERSION, SCHEMA_HASH};
use dsa_protocol_schema::schema_hash;
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

fn secret(byte: u8) -> BootSecret {
    BootSecret::from_bytes([byte; 32])
}

fn pair() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let (mut listener, mut connector) = memory::pair();
    let guest = connector.connect(WAIT).unwrap();
    let host = listener.accept(WAIT).unwrap();
    (host, guest)
}

type Outcome = Result<dsa_protocol::control::Hello, HandshakeError>;

/// Runs the host side in a thread and the guest side here, both knowing the same secret.
fn handshake(guest_hello: dsa_protocol::control::Hello) -> (Outcome, Outcome) {
    handshake_with(guest_hello, secret(7), secret(7))
}

fn handshake_with(
    guest_hello: dsa_protocol::control::Hello,
    host_secret: BootSecret,
    guest_secret: BootSecret,
) -> (Outcome, Outcome) {
    let (mut host, mut guest) = pair();
    let host_side = thread::spawn(move || {
        let local = local_hello(Role::Host, "dsa-host test");
        host_accept(host.as_mut(), &local, &host_secret, WAIT)
    });
    let guest_result = guest_connect(guest.as_mut(), &guest_hello, &guest_secret, WAIT);
    (host_side.join().unwrap(), guest_result)
}

fn rejection(result: &Outcome) -> (RejectReason, String) {
    match result {
        Err(HandshakeError::Rejected { reason, detail }) => (*reason, detail.clone()),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn pr2_peers_built_from_the_same_definition_connect() {
    let (host, guest) = handshake(local_hello(Role::Guest, "dsa-guestd test"));
    let guest_seen_by_host = host.expect("the host accepts");
    let host_seen_by_guest = guest.expect("the guest is accepted");
    assert_eq!(guest_seen_by_host.build, "dsa-guestd test");
    assert_eq!(host_seen_by_guest.build, "dsa-host test");
    assert_eq!(host_seen_by_guest.schema_hash, SCHEMA_HASH);
}

#[test]
fn pr2_a_peer_regenerated_from_a_changed_definition_is_refused() {
    // The guest side regenerated from an edited control.proto; the host was not.
    let definition =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../control.proto"))
            .unwrap();
    // Independent of line endings, since a Windows checkout has CRLF.
    let edited = definition.replace("string build = 4;", "string build = 4; uint32 extra = 5;");
    assert_ne!(edited, definition, "the edit must apply");
    let mut guest = local_hello(Role::Guest, "dsa-guestd edited");
    guest.schema_hash = schema_hash(&edited).to_vec();

    let (host, guest) = handshake(guest);
    let (host_reason, host_detail) = rejection(&host);
    let (guest_reason, guest_detail) = rejection(&guest);
    assert_eq!(host_reason, RejectReason::SchemaHashMismatch);
    assert_eq!(guest_reason, RejectReason::SchemaHashMismatch);
    assert_eq!(
        host_detail, guest_detail,
        "the guest learns the host's reason"
    );
    assert!(
        host_detail.contains("different protocol definition"),
        "{host_detail}"
    );
    assert!(
        host_detail.contains(&dsa_protocol::SCHEMA_HASH_HEX[..12]),
        "{host_detail}"
    );
}

#[test]
fn pr2_a_different_protocol_version_is_refused() {
    let mut guest = local_hello(Role::Guest, "dsa-guestd old");
    guest.protocol_version = PROTOCOL_VERSION + 1;
    let (host, guest) = handshake(guest);
    assert_eq!(rejection(&host).0, RejectReason::ProtocolVersionMismatch);
    assert_eq!(rejection(&guest).0, RejectReason::ProtocolVersionMismatch);
}

#[test]
fn pr2_a_peer_claiming_the_host_role_is_refused() {
    let (host, _guest) = handshake(local_hello(Role::Host, "impostor"));
    let (reason, detail) = rejection(&host);
    assert_eq!(reason, RejectReason::UnexpectedRole);
    assert!(detail.contains("says it is the host"), "{detail}");
}

#[test]
fn pr2_a_truncated_schema_hash_is_refused() {
    let mut guest = local_hello(Role::Guest, "short hash");
    guest.schema_hash.truncate(4);
    let (host, _guest) = handshake(guest);
    assert_eq!(rejection(&host).0, RejectReason::MalformedHello);
}

#[test]
fn pr2_garbage_instead_of_a_hello_is_refused_and_the_connection_closed() {
    let (mut host, mut guest) = pair();
    guest.send(&[0xff, 0x00, 0x13]).unwrap();
    let local = local_hello(Role::Host, "dsa-host test");
    assert_eq!(
        rejection(&host_accept(host.as_mut(), &local, &secret(7), WAIT)).0,
        RejectReason::MalformedHello
    );
    // The guest still receives the refusal, then end of stream.
    let refusal = dsa_protocol::decode(&guest.recv(Some(WAIT)).unwrap()).unwrap();
    assert!(matches!(
        refusal.body,
        Some(dsa_protocol::control::envelope::Body::HelloRejected(_))
    ));
    assert_eq!(guest.recv(Some(WAIT)), Err(TransportError::Closed));
}

#[test]
fn pr2_a_first_message_that_is_not_a_hello_is_refused() {
    let (mut host, mut guest) = pair();
    let not_hello = dsa_protocol::control::Envelope { body: None };
    guest.send(&encode(&not_hello)).unwrap();
    let local = local_hello(Role::Host, "dsa-host test");
    let (reason, detail) = rejection(&host_accept(host.as_mut(), &local, &secret(7), WAIT));
    assert_eq!(reason, RejectReason::MalformedHello);
    assert!(detail.contains("not a Hello"), "{detail}");
}

#[test]
fn pr2_a_silent_peer_times_out() {
    let (mut host, _guest) = pair();
    let local = local_hello(Role::Host, "dsa-host test");
    assert_eq!(
        host_accept(host.as_mut(), &local, &secret(7), Duration::from_millis(50)),
        Err(HandshakeError::Transport(TransportError::Timeout))
    );
}

/// Sends the guest's Hello by hand and returns the host's challenge nonce.
fn challenge(guest: &mut dyn Connection) -> Vec<u8> {
    let hello = Envelope {
        body: Some(Body::Hello(local_hello(Role::Guest, "by hand"))),
    };
    guest.send(&encode(&hello)).unwrap();
    match dsa_protocol::decode(&guest.recv(Some(WAIT)).unwrap())
        .unwrap()
        .body
    {
        Some(Body::AuthChallenge(challenge)) => challenge.nonce,
        other => panic!("expected a challenge, got {other:?}"),
    }
}

fn send_proof(guest: &mut dyn Connection, mac: Vec<u8>) {
    let proof = Envelope {
        body: Some(Body::AuthProof(AuthProof { mac })),
    };
    guest.send(&encode(&proof)).unwrap();
}

#[test]
fn pr6_a_guest_with_the_wrong_secret_is_refused() {
    let (host, guest) = handshake_with(local_hello(Role::Guest, "impostor"), secret(7), secret(8));
    let (reason, detail) = rejection(&host);
    assert_eq!(reason, RejectReason::AuthenticationFailed);
    assert_eq!(rejection(&guest).0, RejectReason::AuthenticationFailed);
    // Says what failed without echoing anything secret.
    assert!(detail.contains("secret"), "{detail}");
    assert!(!detail.contains(&secret(7).to_hex()), "{detail}");
}

#[test]
fn pr6_a_correct_proof_is_accepted_and_the_secret_never_crosses_the_connection() {
    let (mut host, mut guest) = pair();
    let host_side = thread::spawn(move || {
        let local = local_hello(Role::Host, "dsa-host test");
        host_accept(host.as_mut(), &local, &secret(7), WAIT)
    });
    let nonce = challenge(guest.as_mut());
    assert_eq!(nonce.len(), 32);
    assert_ne!(nonce, vec![7u8; 32]);
    send_proof(guest.as_mut(), secret(7).proof(&nonce).to_vec());
    host_side
        .join()
        .unwrap()
        .expect("a correct proof is accepted");
}

#[test]
fn pr6_a_proof_replayed_on_a_new_connection_is_refused() {
    // A proof seen on one connection answers a nonce the host never asks again.
    let (mut listener, mut connector) = memory::pair();
    let host_side = thread::spawn(move || {
        let local = local_hello(Role::Host, "dsa-host test");
        let mut results = Vec::new();
        for _ in 0..2 {
            let mut connection = listener.accept(WAIT).unwrap();
            results.push(host_accept(connection.as_mut(), &local, &secret(7), WAIT));
        }
        results
    });
    let mut first = connector.connect(WAIT).unwrap();
    let first_nonce = challenge(first.as_mut());
    let seen = secret(7).proof(&first_nonce).to_vec();
    send_proof(first.as_mut(), seen.clone());

    let mut second = connector.connect(WAIT).unwrap();
    let second_nonce = challenge(second.as_mut());
    assert_ne!(
        first_nonce, second_nonce,
        "each connection gets its own nonce"
    );
    send_proof(second.as_mut(), seen);

    let results = host_side.join().unwrap();
    assert!(results[0].is_ok(), "{:?}", results[0]);
    assert_eq!(rejection(&results[1]).0, RejectReason::AuthenticationFailed);
}

#[test]
fn pr6_a_proof_keyed_with_an_earlier_boots_secret_is_refused() {
    let (mut host, mut guest) = pair();
    let host_side = thread::spawn(move || {
        let local = local_hello(Role::Host, "dsa-host test");
        host_accept(host.as_mut(), &local, &secret(9), WAIT)
    });
    let nonce = challenge(guest.as_mut());
    // The right nonce, but keyed with the secret the host gave the previous boot.
    send_proof(guest.as_mut(), secret(7).proof(&nonce).to_vec());
    assert_eq!(
        rejection(&host_side.join().unwrap()).0,
        RejectReason::AuthenticationFailed
    );
    let refusal = dsa_protocol::decode(&guest.recv(Some(WAIT)).unwrap()).unwrap();
    assert!(matches!(refusal.body, Some(Body::HelloRejected(_))));
    assert_eq!(guest.recv(Some(WAIT)), Err(TransportError::Closed));
}

#[test]
fn pr6_something_other_than_a_proof_is_refused() {
    let (mut host, mut guest) = pair();
    let host_side = thread::spawn(move || {
        let local = local_hello(Role::Host, "dsa-host test");
        host_accept(host.as_mut(), &local, &secret(7), WAIT)
    });
    challenge(guest.as_mut());
    let hello_again = Envelope {
        body: Some(Body::Hello(local_hello(Role::Guest, "again"))),
    };
    guest.send(&encode(&hello_again)).unwrap();
    assert_eq!(
        rejection(&host_side.join().unwrap()).0,
        RejectReason::AuthenticationFailed
    );
}

#[test]
fn pr6_the_proof_is_hmac_sha256_of_the_nonce() {
    // RFC 4231, test case 2.
    let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        dsa_protocol::auth::to_hex(&mac),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    // RFC 4231, test case 6: a key longer than the block is hashed first.
    let mac = hmac_sha256(
        &[0xaa; 131],
        b"Test Using Larger Than Block-Size Key - Hash Key First",
    );
    assert_eq!(
        dsa_protocol::auth::to_hex(&mac),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
    let nonce = [3u8; 32];
    assert_eq!(secret(7).proof(&nonce), hmac_sha256(&[7u8; 32], &nonce));
}

#[test]
fn pr6_the_secret_round_trips_through_the_guest_bootconfig() {
    let original = BootSecret::generate().unwrap();
    assert_ne!(
        original,
        BootSecret::generate().unwrap(),
        "each session gets a new secret"
    );
    let bootconfig = format!(
        "androidboot.hardware = \"cutf_cvm\"\n{}androidboot.slot_suffix = \"_a\"\n",
        dsa_protocol::auth::bootconfig_entry(&original)
    );
    assert_eq!(
        dsa_protocol::auth::secret_from_bootconfig(&bootconfig),
        Some(original.clone())
    );
    assert_eq!(
        dsa_protocol::auth::secret_from_bootconfig("androidboot.x = \"1\"\n"),
        None
    );
    // Android turns androidboot.* into properties every app can read.
    assert!(!dsa_protocol::auth::bootconfig_entry(&original).starts_with("androidboot."));
    // Nor does it show up in a log by accident.
    assert!(!format!("{original:?}").contains(&original.to_hex()));
}
