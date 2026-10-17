//! The connection handshake over the in-memory transport.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dsa_protocol::control::{RejectReason, Role};
use dsa_protocol::handshake::{guest_connect, host_accept, local_hello, HandshakeError};
use dsa_protocol::{encode, PROTOCOL_VERSION, SCHEMA_HASH};
use dsa_protocol_schema::schema_hash;
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

fn pair() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let (mut listener, mut connector) = memory::pair();
    let guest = connector.connect(WAIT).unwrap();
    let host = listener.accept(WAIT).unwrap();
    (host, guest)
}

/// Runs the host side in a thread and the guest side here.
fn handshake(
    guest_hello: dsa_protocol::control::Hello,
) -> (
    Result<dsa_protocol::control::Hello, HandshakeError>,
    Result<dsa_protocol::control::Hello, HandshakeError>,
) {
    let (mut host, mut guest) = pair();
    let host_side = thread::spawn(move || {
        let local = local_hello(Role::Host, "dsa-host test");
        host_accept(host.as_mut(), &local, WAIT)
    });
    let guest_result = guest_connect(guest.as_mut(), &guest_hello, WAIT);
    (host_side.join().unwrap(), guest_result)
}

fn rejection(
    result: &Result<dsa_protocol::control::Hello, HandshakeError>,
) -> (RejectReason, String) {
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
        rejection(&host_accept(host.as_mut(), &local, WAIT)).0,
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
    let (reason, detail) = rejection(&host_accept(host.as_mut(), &local, WAIT));
    assert_eq!(reason, RejectReason::MalformedHello);
    assert!(detail.contains("not a Hello"), "{detail}");
}

#[test]
fn pr2_a_silent_peer_times_out() {
    let (mut host, _guest) = pair();
    let local = local_hello(Role::Host, "dsa-host test");
    assert_eq!(
        host_accept(host.as_mut(), &local, Duration::from_millis(50)),
        Err(HandshakeError::Transport(TransportError::Timeout))
    );
}
