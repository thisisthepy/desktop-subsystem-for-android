//! The local API's handshake and the hash its generated code carries.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dsa_protocol::host_api::types::{envelope::Body, Envelope, RejectReason, Role};
use dsa_protocol::host_api::{
    client_connect, decode, local_hello, service_accept, HandshakeError, SCHEMA_HASH,
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

/// Runs the service's side of the handshake on a thread, the client's on this one.
fn handshake(
    client_hello: dsa_protocol::host_api::types::Hello,
) -> (
    Result<dsa_protocol::host_api::types::Hello, HandshakeError>,
    Result<dsa_protocol::host_api::types::Hello, HandshakeError>,
) {
    let (mut service, mut client) = connected();
    let service = thread::spawn(move || {
        service_accept(&mut *service, &local_hello(Role::Service, "service"), WAIT)
    });
    let client = client_connect(&mut *client, &client_hello, WAIT);
    (service.join().unwrap(), client)
}

fn protocol_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn pr5_the_generated_hash_matches_the_definition() {
    let definition = std::fs::read_to_string(protocol_dir().join("host_api.proto")).unwrap();
    assert_eq!(
        schema_hash(&definition),
        SCHEMA_HASH,
        "host_api.proto changed without regenerating; run dsa-protocol-codegen"
    );
    assert_eq!(to_hex(&SCHEMA_HASH), SCHEMA_HASH_HEX);
    let file = std::fs::read_to_string(protocol_dir().join("host_api.sha256")).unwrap();
    assert_eq!(file.trim(), SCHEMA_HASH_HEX);
}

#[test]
fn pr5_a_client_from_the_same_definition_connects() {
    let (service, client) = handshake(local_hello(Role::Client, "client"));
    assert_eq!(service.unwrap().build, "client");
    assert_eq!(client.unwrap().build, "service");
}

#[test]
fn pr5_a_client_from_a_different_definition_is_refused_with_both_hashes() {
    let mut hello = local_hello(Role::Client, "old client");
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
fn pr5_a_client_of_another_version_is_refused() {
    let mut hello = local_hello(Role::Client, "client");
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
fn pr5_a_peer_that_claims_to_be_the_service_is_refused() {
    let (service, _) = handshake(local_hello(Role::Service, "impostor"));
    assert!(matches!(
        service,
        Err(HandshakeError::Rejected {
            reason: RejectReason::UnexpectedRole,
            ..
        })
    ));
}

#[test]
fn pr5_garbage_instead_of_a_hello_is_refused_and_the_connection_closed() {
    let (mut service, mut client) = connected();
    client.send(&[0xff, 0xff, 0xff]).unwrap();
    let result = service_accept(&mut *service, &local_hello(Role::Service, "service"), WAIT);
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
