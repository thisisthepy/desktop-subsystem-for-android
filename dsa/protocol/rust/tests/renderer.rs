//! The graphics renderer process's handshake with the window manager, the hash its generated
//! code carries, and the messages that carry a frame between them.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use dsa_protocol::renderer::types::{
    envelope::Body, DeviceState, Envelope, FrameDone, RejectReason, Role, Target, TargetRequest,
};
use dsa_protocol::renderer::{
    decode, decode_body, encode_body, local_hello, renderer_connect, window_manager_accept,
    HandshakeError, SCHEMA_HASH, SCHEMA_HASH_HEX,
};
use dsa_protocol_schema::{schema_hash, to_hex};
use dsa_transport::{memory, Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);

type Hello = dsa_protocol::renderer::types::Hello;

fn connected() -> (Box<dyn Connection>, Box<dyn Connection>) {
    let (mut listener, mut connector) = memory::pair();
    let renderer = connector.connect(WAIT).unwrap();
    let window_manager = listener.accept(WAIT).unwrap();
    (window_manager, renderer)
}

/// Runs the window manager's side of the handshake on a thread, the renderer's on this one.
fn handshake(
    renderer_hello: Hello,
) -> (Result<Hello, HandshakeError>, Result<Hello, HandshakeError>) {
    let (mut window_manager, mut renderer) = connected();
    let window_manager = thread::spawn(move || {
        window_manager_accept(
            &mut *window_manager,
            &local_hello(Role::WindowManager, "dsa-wm"),
            WAIT,
        )
    });
    let renderer = renderer_connect(&mut *renderer, &renderer_hello, WAIT);
    (window_manager.join().unwrap(), renderer)
}

fn protocol_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn pr2_the_renderer_hash_matches_its_definition() {
    let definition = std::fs::read_to_string(protocol_dir().join("renderer.proto")).unwrap();
    assert_eq!(
        schema_hash(&definition),
        SCHEMA_HASH,
        "renderer.proto changed without regenerating; run dsa-protocol-codegen"
    );
    assert_eq!(to_hex(&SCHEMA_HASH), SCHEMA_HASH_HEX);
    let file = std::fs::read_to_string(protocol_dir().join("renderer.sha256")).unwrap();
    assert_eq!(file.trim(), SCHEMA_HASH_HEX);
}

#[test]
fn nfr5_a_renderer_from_the_same_definition_connects() {
    let (window_manager, renderer) = handshake(local_hello(Role::Renderer, "dsa-gfx"));
    assert_eq!(window_manager.unwrap().build, "dsa-gfx");
    assert_eq!(renderer.unwrap().build, "dsa-wm");
}

#[test]
fn pr2_a_renderer_from_a_different_definition_is_refused_with_both_hashes() {
    let mut hello = local_hello(Role::Renderer, "old dsa-gfx");
    hello.schema_hash[0] ^= 0xff;
    let (window_manager, renderer) = handshake(hello.clone());
    for result in [window_manager, renderer] {
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
fn pr2_a_renderer_of_another_version_is_refused() {
    let mut hello = local_hello(Role::Renderer, "dsa-gfx");
    hello.protocol_version += 1;
    let (window_manager, renderer) = handshake(hello);
    for result in [window_manager, renderer] {
        assert!(matches!(
            result,
            Err(HandshakeError::Rejected {
                reason: RejectReason::ProtocolVersionMismatch,
                ..
            })
        ));
    }
}

#[test]
fn pr2_a_peer_that_claims_to_be_the_window_manager_is_refused_as_a_renderer() {
    let (window_manager, _) = handshake(local_hello(Role::WindowManager, "impostor"));
    assert!(matches!(
        window_manager,
        Err(HandshakeError::Rejected {
            reason: RejectReason::UnexpectedRole,
            ..
        })
    ));
}

#[test]
fn pr2_garbage_instead_of_a_renderers_hello_is_refused_and_the_connection_closed() {
    let (mut window_manager, mut renderer) = connected();
    renderer.send(&[0xff, 0xff, 0xff]).unwrap();
    let result = window_manager_accept(
        &mut *window_manager,
        &local_hello(Role::WindowManager, "dsa-wm"),
        WAIT,
    );
    assert!(matches!(
        result,
        Err(HandshakeError::Rejected {
            reason: RejectReason::MalformedHello,
            ..
        })
    ));
    let refusal = decode(&renderer.recv(Some(WAIT)).unwrap()).unwrap();
    assert!(matches!(
        refusal,
        Envelope {
            body: Some(Body::HelloRejected(_))
        }
    ));
}

#[test]
fn nfr5_a_frames_request_its_target_and_its_completion_survive_the_wire() {
    let request = Body::TargetRequest(TargetRequest {
        request: 7,
        display_id: 3,
        width: 1920,
        height: 1080,
        exact: true,
        held_textures: vec![11, 12],
    });
    assert_eq!(decode_body(&encode_body(request.clone())).unwrap(), request);
    let target = Body::Target(Target {
        request: 7,
        drop: false,
        texture_id: 12,
        handle: 0x1a4,
        width: 1920,
        height: 1080,
    });
    assert_eq!(decode_body(&encode_body(target.clone())).unwrap(), target);
    let done = Body::FrameDone(FrameDone {
        display_id: 3,
        texture_id: 12,
        width: 800,
        height: 600,
    });
    assert_eq!(decode_body(&encode_body(done.clone())).unwrap(), done);
    // An envelope with no body is an error, not a panic.
    assert!(decode_body(&[]).is_err());
}

#[test]
fn nfr5_the_renderers_device_state_survives_the_wire() {
    let state = Body::DeviceState(DeviceState {
        losses: 2,
        generation: 1,
    });
    assert_eq!(decode_body(&encode_body(state.clone())).unwrap(), state);
}
