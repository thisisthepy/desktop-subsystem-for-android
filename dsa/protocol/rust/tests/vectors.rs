//! The checked-in protocol vectors, against the generated Rust codec.
//!
//! `dsa/protocol/vectors/control.vectors` holds, for each named case, the bytes of one
//! Envelope. The Java and C++ tests build the same cases and must produce the same bytes.
//! Set `DSA_WRITE_VECTORS=1` to rewrite the file from the cases below (then rerun every
//! language's tests).

use std::collections::BTreeMap;
use std::path::PathBuf;

use dsa_protocol::control::{
    envelope::Body, Envelope, Hello, HelloAccepted, HelloRejected, RejectReason, Role,
};
use dsa_protocol::{decode, encode};

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/control.vectors")
}

fn guest_hello() -> Hello {
    Hello {
        protocol_version: 1,
        schema_hash: (0u8..32).collect(),
        role: Role::Guest as i32,
        build: "dsa-guestd 0.1.0".into(),
    }
}

fn host_hello() -> Hello {
    Hello {
        protocol_version: 1,
        schema_hash: (0xa0u8..0xc0).collect(),
        role: Role::Host as i32,
        build: "dsa-host 0.1.0".into(),
    }
}

/// Every case, by name. The other languages' tests mirror this list exactly.
fn cases() -> BTreeMap<&'static str, Envelope> {
    let wrap = |body| Envelope { body: Some(body) };
    BTreeMap::from([
        ("envelope_empty", Envelope { body: None }),
        ("hello_default", wrap(Body::Hello(Hello::default()))),
        ("hello_guest", wrap(Body::Hello(guest_hello()))),
        (
            "hello_max_version",
            wrap(Body::Hello(Hello {
                protocol_version: u32::MAX,
                role: Role::Guest as i32,
                ..Hello::default()
            })),
        ),
        (
            "hello_accepted",
            wrap(Body::HelloAccepted(HelloAccepted {
                host: Some(host_hello()),
            })),
        ),
        (
            "hello_rejected",
            wrap(Body::HelloRejected(HelloRejected {
                reason: RejectReason::SchemaHashMismatch as i32,
                detail: "스키마가 다릅니다 (schema differs)".into(),
                host: Some(host_hello()),
            })),
        ),
    ])
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

fn read_vectors() -> BTreeMap<String, Vec<u8>> {
    // Tests run in parallel; in write mode the file is rewritten once, before any read.
    static WRITE: std::sync::Once = std::sync::Once::new();
    if std::env::var_os("DSA_WRITE_VECTORS").is_some() {
        WRITE.call_once(write_vectors);
    }
    let text = std::fs::read_to_string(vectors_path()).expect("reading control.vectors");
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next().unwrap().to_string();
            let hex = parts.next().unwrap_or("-");
            (
                name,
                if hex == "-" {
                    Vec::new()
                } else {
                    from_hex(hex)
                },
            )
        })
        .collect()
}

fn write_vectors() {
    let mut text = String::from(
        "# Control protocol vectors. Each line: a case name, then the bytes of one Envelope in\n\
         # hex (\"-\" for none). Every language's generated codec must encode each case to\n\
         # exactly these bytes and decode them back to the same message.\n",
    );
    for (name, envelope) in cases() {
        let bytes = encode(&envelope);
        let hex = if bytes.is_empty() {
            "-".to_string()
        } else {
            to_hex(&bytes)
        };
        text.push_str(&format!("{name} {hex}\n"));
    }
    std::fs::write(vectors_path(), text).expect("writing control.vectors");
}

#[test]
fn pr2_the_rust_codec_encodes_every_vector_byte_for_byte() {
    let vectors = read_vectors();
    let cases = cases();
    assert_eq!(
        vectors.keys().map(String::as_str).collect::<Vec<_>>(),
        cases.keys().copied().collect::<Vec<_>>(),
        "the vector file and the cases list different names"
    );
    for (name, envelope) in &cases {
        assert_eq!(
            to_hex(&encode(envelope)),
            to_hex(&vectors[*name]),
            "encoding {name}"
        );
    }
}

#[test]
fn pr2_the_rust_codec_decodes_every_vector_to_the_same_message() {
    let vectors = read_vectors();
    for (name, envelope) in cases() {
        assert_eq!(
            decode(&vectors[name]).expect(name),
            envelope,
            "decoding {name}"
        );
    }
}

#[test]
fn pr2_malformed_frames_are_errors_not_panics() {
    for frame in [
        vec![0xff],
        vec![0x0a, 0x05, 0x01],
        vec![0x0a; 64],
        vec![0x80; 11],
    ] {
        assert!(decode(&frame).is_err(), "{frame:?} should not decode");
    }
}
