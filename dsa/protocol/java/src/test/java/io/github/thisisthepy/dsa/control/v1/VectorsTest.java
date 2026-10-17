package io.github.thisisthepy.dsa.control.v1;

import com.google.protobuf.ByteString;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

/**
 * The checked-in protocol vectors, against the generated Java (protobuf-lite) codec.
 *
 * <p>The cases mirror dsa/protocol/rust/tests/vectors.rs exactly. Run with
 * dsa/protocol/java/test.sh, which passes the protocol folder as the only argument.
 */
public final class VectorsTest {
    private static final List<String> failures = new ArrayList<>();

    private static Hello guestHello() {
        byte[] hash = new byte[32];
        for (int i = 0; i < 32; i++) hash[i] = (byte) i;
        return Hello.newBuilder()
                .setProtocolVersion(1)
                .setSchemaHash(ByteString.copyFrom(hash))
                .setRole(Role.ROLE_GUEST)
                .setBuild("dsa-guestd 0.1.0")
                .build();
    }

    private static Hello hostHello() {
        byte[] hash = new byte[32];
        for (int i = 0; i < 32; i++) hash[i] = (byte) (0xa0 + i);
        return Hello.newBuilder()
                .setProtocolVersion(1)
                .setSchemaHash(ByteString.copyFrom(hash))
                .setRole(Role.ROLE_HOST)
                .setBuild("dsa-host 0.1.0")
                .build();
    }

    private static Map<String, Envelope> cases() {
        Map<String, Envelope> cases = new TreeMap<>();
        cases.put("envelope_empty", Envelope.getDefaultInstance());
        cases.put("hello_default", Envelope.newBuilder().setHello(Hello.getDefaultInstance()).build());
        cases.put("hello_guest", Envelope.newBuilder().setHello(guestHello()).build());
        cases.put("hello_max_version", Envelope.newBuilder()
                .setHello(Hello.newBuilder().setProtocolVersion(0xffffffff).setRole(Role.ROLE_GUEST))
                .build());
        cases.put("hello_accepted", Envelope.newBuilder()
                .setHelloAccepted(HelloAccepted.newBuilder().setHost(hostHello()))
                .build());
        cases.put("hello_rejected", Envelope.newBuilder()
                .setHelloRejected(HelloRejected.newBuilder()
                        .setReason(RejectReason.REJECT_REASON_SCHEMA_HASH_MISMATCH)
                        .setDetail("스키마가 다릅니다 (schema differs)")
                        .setHost(hostHello()))
                .build());
        return cases;
    }

    private static Map<String, byte[]> readVectors(Path file) throws Exception {
        Map<String, byte[]> vectors = new TreeMap<>();
        for (String line : Files.readAllLines(file, StandardCharsets.UTF_8)) {
            if (line.isBlank() || line.startsWith("#")) continue;
            String[] parts = line.trim().split("\\s+");
            String hex = parts.length > 1 ? parts[1] : "-";
            vectors.put(parts[0], hex.equals("-") ? new byte[0] : fromHex(hex));
        }
        return vectors;
    }

    private static byte[] fromHex(String hex) {
        byte[] bytes = new byte[hex.length() / 2];
        for (int i = 0; i < bytes.length; i++) {
            bytes[i] = (byte) Integer.parseInt(hex.substring(2 * i, 2 * i + 2), 16);
        }
        return bytes;
    }

    private static String toHex(byte[] bytes) {
        StringBuilder text = new StringBuilder();
        for (byte b : bytes) text.append(String.format("%02x", b & 0xff));
        return text.toString();
    }

    private static void check(boolean ok, String what) {
        if (!ok) failures.add(what);
    }

    static void pr2_the_java_codec_encodes_every_vector_byte_for_byte(Map<String, byte[]> vectors) {
        check(vectors.keySet().equals(cases().keySet()),
                "the vector file lists " + vectors.keySet() + " but the cases are " + cases().keySet());
        for (Map.Entry<String, Envelope> c : cases().entrySet()) {
            byte[] expected = vectors.get(c.getKey());
            byte[] actual = c.getValue().toByteArray();
            check(expected != null && Arrays.equals(expected, actual),
                    "encoding " + c.getKey() + ": got " + toHex(actual)
                            + ", expected " + (expected == null ? "nothing" : toHex(expected)));
        }
    }

    static void pr2_the_java_codec_decodes_every_vector_to_the_same_message(Map<String, byte[]> vectors)
            throws Exception {
        for (Map.Entry<String, Envelope> c : cases().entrySet()) {
            byte[] bytes = vectors.get(c.getKey());
            if (bytes == null) continue;
            check(Envelope.parseFrom(bytes).equals(c.getValue()), "decoding " + c.getKey());
        }
    }

    static void pr2_the_java_code_carries_the_shared_schema_hash(Path protocol) throws Exception {
        String shared = Files.readString(protocol.resolve("schema.sha256"), StandardCharsets.UTF_8).trim();
        check(ControlSchema.SCHEMA_HASH_HEX.equals(shared),
                "ControlSchema.SCHEMA_HASH_HEX is " + ControlSchema.SCHEMA_HASH_HEX
                        + " but schema.sha256 is " + shared + "; regenerate with dsa-protocol-codegen");
    }

    public static void main(String[] args) throws Exception {
        Path protocol = Path.of(args[0]);
        Map<String, byte[]> vectors = readVectors(protocol.resolve("vectors/control.vectors"));
        pr2_the_java_codec_encodes_every_vector_byte_for_byte(vectors);
        pr2_the_java_codec_decodes_every_vector_to_the_same_message(vectors);
        pr2_the_java_code_carries_the_shared_schema_hash(protocol);
        if (!failures.isEmpty()) {
            failures.forEach(f -> System.err.println("FAILED: " + f));
            System.exit(1);
        }
        System.out.println("java vectors: " + cases().size() + " cases passed");
    }
}
