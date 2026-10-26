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

    /** {@code count} bytes counting up from {@code first}. */
    private static byte[] bytes(int first, int count) {
        byte[] out = new byte[count];
        for (int i = 0; i < count; i++) out[i] = (byte) (first + i);
        return out;
    }

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
        cases.put("auth_challenge", Envelope.newBuilder()
                .setAuthChallenge(AuthChallenge.newBuilder().setNonce(ByteString.copyFrom(bytes(0x40, 32))))
                .build());
        cases.put("auth_proof", Envelope.newBuilder()
                .setAuthProof(AuthProof.newBuilder().setMac(ByteString.copyFrom(bytes(0x60, 32))))
                .build());
        cases.put("task_updated", Envelope.newBuilder()
                .setTaskUpdated(TaskUpdated.newBuilder()
                        .setTaskId(42)
                        .setBounds(Rect.newBuilder().setLeft(100).setTop(80).setRight(1180).setBottom(800))
                        .setPackageName("com.example.notes")
                        .setLabel("메모 (Notes)")
                        .setFocused(true))
                .build());
        cases.put("task_updated_density", Envelope.newBuilder()
                .setTaskUpdated(TaskUpdated.newBuilder()
                        .setTaskId(42)
                        .setBounds(Rect.newBuilder().setLeft(0).setTop(0).setRight(1096).setBottom(1948))
                        .setPackageName("com.example.notes")
                        .setDensityDpi(320)
                        .setMaxWidth(3840)
                        .setMaxHeight(2160))
                .build());
        cases.put("resize_task", Envelope.newBuilder()
                .setResizeTask(ResizeTask.newBuilder().setTaskId(42).setWidth(1280).setHeight(720))
                .build());
        cases.put("set_task_density", Envelope.newBuilder()
                .setSetTaskDensity(SetTaskDensity.newBuilder().setTaskId(42).setDensityDpi(240))
                .build());
        cases.put("task_updated_negative_origin", Envelope.newBuilder()
                .setTaskUpdated(TaskUpdated.newBuilder()
                        .setTaskId(7)
                        .setBounds(Rect.newBuilder().setLeft(-40).setTop(-10).setRight(600).setBottom(400)))
                .build());
        cases.put("task_removed", Envelope.newBuilder()
                .setTaskRemoved(TaskRemoved.newBuilder().setTaskId(42))
                .build());
        cases.put("task_frames", Envelope.newBuilder()
                .setTaskFrames(TaskFrames.newBuilder().setTaskId(42).setDisplayId(3))
                .build());
        cases.put("close_task", Envelope.newBuilder()
                .setCloseTask(CloseTask.newBuilder().setTaskId(42))
                .build());
        cases.put("launch_app", Envelope.newBuilder()
                .setLaunchApp(LaunchApp.newBuilder().setPackageName("com.example.notes"))
                .build());
        cases.put("app_info", Envelope.newBuilder()
                .setAppInfo(AppInfo.newBuilder()
                        .setPackageName("com.example.notes")
                        .setLabel("Notes")
                        .setIconPng(ByteString.copyFrom(
                                new byte[] {(byte) 0x89, 'P', 'N', 'G', 0x0d, 0x0a, 0x1a, 0x0a})))
                .build());
        cases.put("pointer_event_mouse_secondary_down", Envelope.newBuilder()
                .setPointerEvent(PointerEvent.newBuilder()
                        .setTaskId(42)
                        .setAction(PointerAction.POINTER_ACTION_DOWN)
                        .setTool(PointerTool.POINTER_TOOL_MOUSE)
                        .setPointerId(0)
                        .setX(120.5f)
                        .setY(64.25f)
                        .setButtons(2)
                        .setPressure(1.0f))
                .build());
        cases.put("pointer_event_touch_move", Envelope.newBuilder()
                .setPointerEvent(PointerEvent.newBuilder()
                        .setTaskId(7)
                        .setAction(PointerAction.POINTER_ACTION_MOVE)
                        .setTool(PointerTool.POINTER_TOOL_TOUCH)
                        .setPointerId(3)
                        .setX(10.0f)
                        .setY(1023.75f)
                        .setPressure(0.5f))
                .build());
        cases.put("pointer_event_scroll", Envelope.newBuilder()
                .setPointerEvent(PointerEvent.newBuilder()
                        .setTaskId(42)
                        .setAction(PointerAction.POINTER_ACTION_SCROLL)
                        .setTool(PointerTool.POINTER_TOOL_MOUSE)
                        .setX(300.0f)
                        .setY(200.0f)
                        .setScrollX(0.5f)
                        .setScrollY(-1.0f))
                .build());
        cases.put("pointer_event_pen_hover", Envelope.newBuilder()
                .setPointerEvent(PointerEvent.newBuilder()
                        .setTaskId(42)
                        .setAction(PointerAction.POINTER_ACTION_HOVER_MOVE)
                        .setTool(PointerTool.POINTER_TOOL_PEN)
                        .setPointerId(1)
                        .setX(0.0f)
                        .setY(599.0f)
                        .setButtons(32))
                .build());
        cases.put("hello_rejected_authentication", Envelope.newBuilder()
                .setHelloRejected(HelloRejected.newBuilder()
                        .setReason(RejectReason.REJECT_REASON_AUTHENTICATION_FAILED)
                        .setDetail("no proof"))
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
