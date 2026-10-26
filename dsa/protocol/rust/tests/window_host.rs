//! The window hosts' handshake with the window manager and the hash its generated code
//! carries.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dsa_protocol::window_host::types::{envelope::Body, Envelope, RejectReason, Role};
use dsa_protocol::window_host::{
    decode, local_hello, window_host_connect, window_manager_accept, HandshakeError, SCHEMA_HASH,
    SCHEMA_HASH_HEX,
};
use dsa_protocol_schema::{schema_hash, to_hex};
use dsa_transport::{memory, Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);

fn connected() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let (mut listener, mut connector) = memory::pair();
    let client = connector.connect(WAIT).unwrap();
    let service = listener.accept(WAIT).unwrap();
    (service, client)
}

/// Runs the window manager's side of the handshake on a thread, the window host's on this one.
fn handshake(
    client_hello: dsa_protocol::window_host::types::Hello,
) -> (
    Result<dsa_protocol::window_host::types::Hello, HandshakeError>,
    Result<dsa_protocol::window_host::types::Hello, HandshakeError>,
) {
    let (mut service, mut client) = connected();
    let service = thread::spawn(move || {
        window_manager_accept(
            &mut *service,
            &local_hello(Role::WindowManager, "service"),
            WAIT,
        )
    });
    let client = window_host_connect(&mut *client, &client_hello, WAIT);
    (service.join().unwrap(), client)
}

fn protocol_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn pr2_the_window_host_hash_matches_its_definition() {
    let definition = std::fs::read_to_string(protocol_dir().join("window_host.proto")).unwrap();
    assert_eq!(
        schema_hash(&definition),
        SCHEMA_HASH,
        "window_host.proto changed without regenerating; run dsa-protocol-codegen"
    );
    assert_eq!(to_hex(&SCHEMA_HASH), SCHEMA_HASH_HEX);
    let file = std::fs::read_to_string(protocol_dir().join("window_host.sha256")).unwrap();
    assert_eq!(file.trim(), SCHEMA_HASH_HEX);
}

#[test]
fn pr2_a_window_host_from_the_same_definition_connects() {
    let (service, client) = handshake(local_hello(Role::WindowHost, "client"));
    assert_eq!(service.unwrap().build, "client");
    assert_eq!(client.unwrap().build, "service");
}

#[test]
fn pr2_a_window_host_from_a_different_definition_is_refused_with_both_hashes() {
    let mut hello = local_hello(Role::WindowHost, "old client");
    hello.schema_hash[0] ^= 0xff;
    let (service, client) = handshake(hello.clone());
    for result in [service, client] {
        match result {
            Err(HandshakeError::Rejected { reason, detail }) => {
                assert_eq!(reason, RejectReason::SchemaHashMismatch);
                assert!(
                    detail.contains(&to_hex(&hello.schema_hash)[..12]),
                    "{detail}"
                );
                assert!(detail.contains(&SCHEMA_HASH_HEX[..12]), "{detail}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn pr2_a_window_host_of_another_version_is_refused() {
    let mut hello = local_hello(Role::WindowHost, "client");
    hello.protocol_version += 1;
    let (service, client) = handshake(hello);
    assert!(matches!(
        service,
        Err(HandshakeError::Rejected {
            reason: RejectReason::ProtocolVersionMismatch,
            ..
        })
    ));
    assert!(matches!(
        client,
        Err(HandshakeError::Rejected {
            reason: RejectReason::ProtocolVersionMismatch,
            ..
        })
    ));
}

#[test]
fn pr2_a_peer_that_claims_to_be_the_window_manager_is_refused() {
    let (service, _) = handshake(local_hello(Role::WindowManager, "impostor"));
    assert!(matches!(
        service,
        Err(HandshakeError::Rejected {
            reason: RejectReason::UnexpectedRole,
            ..
        })
    ));
}

#[test]
fn pr2_garbage_instead_of_a_window_hosts_hello_is_refused_and_the_connection_closed() {
    let (mut service, mut client) = connected();
    client.send(&[0xff, 0xff, 0xff]).unwrap();
    let result = window_manager_accept(
        &mut *service,
        &local_hello(Role::WindowManager, "service"),
        WAIT,
    );
    assert!(matches!(
        result,
        Err(HandshakeError::Rejected {
            reason: RejectReason::MalformedHello,
            ..
        })
    ));
    let refusal = decode(&client.recv(Some(WAIT)).unwrap()).unwrap();
    assert!(matches!(
        refusal,
        Envelope {
            body: Some(Body::HelloRejected(_))
        }
    ));
}
