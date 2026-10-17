# protocol

The control channel between the DSA host and the guest: its one definition, the code
generated from it for each language, the checked-in vectors that keep those languages in
step, and the transport that carries the frames.

| Path | What it is |
|---|---|
| `control.proto` | The only definition of the messages (proto3) |
| `schema.sha256` | The schema hash of `control.proto`, written by the generator |
| `schema/` | Rust: computes the schema hash (SHA-256 of the definition's tokens) |
| `codegen/` | Rust: the generator for every language |
| `rust/` | Rust: the generated types, the frame codec and the connection handshake (`dsa-protocol`) |
| `java/` | Java: the generated protobuf-lite classes and the vector test |
| `cpp/` | C++: the generated protobuf-lite sources and the vector test |
| `vectors/control.vectors` | Named messages and their exact bytes, which every language must match |
| `transport/` | Rust: framing, the connection traits and the in-memory fake (`dsa-transport`) |

## Rules

- Generated code is never edited by hand. Change `control.proto` and run the generator.
- Every frame is one `Envelope`. The first frame on a connection is the guest's `Hello`;
  the host answers `HelloAccepted` or `HelloRejected` and refuses a guest whose protocol
  version or schema hash differs from its own, saying which values differ.
- No map fields, no `Any`, no extensions, and nothing that depends on unknown fields, so
  the three languages encode the same message to the same bytes.
- The schema hash ignores comments and layout. Any change to a name, number, type or
  option changes it.

## Regenerating

Needs `protoc` 3.21 (Debian and Ubuntu: `protobuf-compiler`). From `dsa/`:

```
PROTOC=$(command -v protoc) cargo run -p dsa-protocol-codegen
```

This rewrites everything under `rust/src/generated/`, `java/src/main/java/` and
`cpp/generated/`, and `schema.sha256`. CI runs it and fails when the result differs from
what is checked in.

After adding or changing a vector case in `rust/tests/vectors.rs` (and the same case in
the Java and C++ tests), rewrite the vector file from `dsa/`:

```
DSA_WRITE_VECTORS=1 cargo test -p dsa-protocol --test vectors
```

## Testing

From `dsa/`:

- Rust: `cargo test -p dsa-protocol -p dsa-protocol-schema`
- Java: `protocol/java/test.sh` (needs a JDK and the protobuf runtime jar, Debian
  `libprotobuf-java`; `PROTOBUF_JAVA_JAR` points at another jar)
- C++: `protocol/cpp/test.sh` (needs a C++17 compiler and protobuf-lite 3.21, Debian
  `libprotobuf-dev`; `PROTOBUF_PREFIX` points at another installation)

The generated Java and C++ code must be built against the protobuf runtime of the same
version as the `protoc` that generated it.
