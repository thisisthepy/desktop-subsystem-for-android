//! The generated code carries the hash of the definition it came from.

use std::path::PathBuf;

use dsa_protocol::{SCHEMA_HASH, SCHEMA_HASH_HEX};
use dsa_protocol_schema::{schema_hash, to_hex};

fn protocol_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn pr2_the_generated_hash_matches_the_definition() {
    let definition = std::fs::read_to_string(protocol_dir().join("control.proto")).unwrap();
    assert_eq!(
        schema_hash(&definition),
        SCHEMA_HASH,
        "control.proto changed without regenerating; run dsa-protocol-codegen"
    );
    assert_eq!(to_hex(&SCHEMA_HASH), SCHEMA_HASH_HEX);
}

#[test]
fn pr2_the_shared_hash_file_matches_the_rust_code() {
    let file = std::fs::read_to_string(protocol_dir().join("schema.sha256")).unwrap();
    assert_eq!(file.trim(), SCHEMA_HASH_HEX);
}
