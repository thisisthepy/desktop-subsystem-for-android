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
        cases.put("video_capabilities", Envelope.newBuilder()
                .setVideoCapabilities(VideoCapabilities.newBuilder()
                        .addCodecs(VideoCodecSupport.newBuilder()
                                .setCodec(VideoCodec.VIDEO_CODEC_H264)
                                .setMaxWidth(4096)
                                .setMaxHeight(2304)))
                .build());
        cases.put("video_decoder_open", Envelope.newBuilder()
                .setVideoDecoderOpen(VideoDecoderOpen.newBuilder()
                        .setCodec(VideoCodec.VIDEO_CODEC_H264)
                        .setWidth(1280)
                        .setHeight(720))
                .build());
        cases.put("video_flush", Envelope.newBuilder()
                .setVideoFlush(VideoFlush.newBuilder())
                .build());
        cases.put("video_frame", Envelope.newBuilder()
                .setVideoFrame(VideoFrame.newBuilder()
                        .setFrameIndex(3)
                        .setTimestampUs(100000)
                        .setWidth(2)
                        .setHeight(2)
                        .setStride(2)
                        .setNv12(ByteString.copyFrom(new byte[] {0x10, 0x10, 0x10, 0x10, (byte) 0x80, (byte) 0x80})))
                .build());
        cases.put("video_packet", Envelope.newBuilder()
                .setVideoPacket(VideoPacket.newBuilder()
                        .setFrameIndex(3)
                        .setTimestampUs(100000)
                        .setData(ByteString.copyFrom(new byte[] {0, 0, 0, 1, 0x65})))
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
        cases.put("key_event_ctrl_a_down", Envelope.newBuilder()
                .setKeyEvent(KeyEvent.newBuilder()
                        .setTaskId(42)
                        .setAction(KeyAction.KEY_ACTION_DOWN)
                        .setKeyCode(29)
                        .setMetaState(0x3000)
                        .setEventTimeMs(123456))
                .build());
        cases.put("key_event_backspace_repeat", Envelope.newBuilder()
                .setKeyEvent(KeyEvent.newBuilder()
                        .setTaskId(42)
                        .setAction(KeyAction.KEY_ACTION_DOWN)
                        .setKeyCode(67)
                        .setRepeatCount(3)
                        .setEventTimeMs(500))
                .build());
        cases.put("key_event_canceled_up", Envelope.newBuilder()
                .setKeyEvent(KeyEvent.newBuilder()
                        .setTaskId(7)
                        .setAction(KeyAction.KEY_ACTION_UP)
                        .setKeyCode(66)
                        .setMetaState(0x100000)
                        .setEventTimeMs((int) 4294967000L)
                        .setCanceled(true))
                .build());
        cases.put("ime_composition_korean", Envelope.newBuilder()
                .setImeComposition(ImeComposition.newBuilder()
                        .setTaskId(42)
                        .setText("\ud55c")
                        .setCursor(1))
                .build());
        cases.put("ime_composition_removed", Envelope.newBuilder()
                .setImeComposition(ImeComposition.newBuilder()
                        .setTaskId(7))
                .build());
        cases.put("ime_commit_korean", Envelope.newBuilder()
                .setImeCommit(ImeCommit.newBuilder()
                        .setTaskId(42)
                        .setText("\ud55c\uae00"))
                .build());
        cases.put("ime_cursor", Envelope.newBuilder()
                .setImeCursor(ImeCursor.newBuilder()
                        .setTaskId(42)
                        .setCursor(Rect.newBuilder()
                                .setLeft(100)
                                .setTop(200)
                                .setRight(102)
                                .setBottom(240)))
                .build());
        cases.put("screen_state_off", Envelope.newBuilder()
                .setScreenState(ScreenState.newBuilder().setOn(false))
                .build());
        cases.put("screen_state_on", Envelope.newBuilder()
                .setScreenState(ScreenState.newBuilder().setOn(true))
                .build());
        cases.put("power_state_on_battery", Envelope.newBuilder()
                .setPowerState(PowerState.newBuilder()
                        .setAcOnline(false)
                        .setBatteryPresent(true)
                        .setBatteryPercent(57)
                        .setCharging(false)
                        .setPowerSaver(true))
                .build());
        cases.put("power_hint_launch_mode", Envelope.newBuilder()
                .setPowerHint(PowerHint.newBuilder()
                        .setKind(PowerHintKind.POWER_HINT_KIND_MODE)
                        .setId(5)
                        .setEnabled(true))
                .build());
        cases.put("power_hint_interaction_boost", Envelope.newBuilder()
                .setPowerHint(PowerHint.newBuilder()
                        .setKind(PowerHintKind.POWER_HINT_KIND_BOOST)
                        .setId(0)
                        .setDurationMs(80))
                .build());
        cases.put("device_idle", Envelope.newBuilder()
                .setDeviceIdle(DeviceIdle.newBuilder().setIdle(true))
                .build());
        cases.put("clipboard_text", Envelope.newBuilder()
                .setClipboardText(ClipboardText.newBuilder().setText("hello \uc548\ub155"))
                .build());
        cases.put("clipboard_sharing_off", Envelope.newBuilder()
                .setClipboardSharing(ClipboardSharing.newBuilder().setOn(false))
                .build());
        String chatKey = "0|com.example.chat|1|null|10100";
        cases.put("notification", Envelope.newBuilder()
                .setNotification(GuestNotification.newBuilder()
                        .setKey(chatKey)
                        .setPackageName("com.example.chat")
                        .setAppLabel("Chat")
                        .setTitle("Ann")
                        .setText("\uc548\ub155")
                        .setIconPng(com.google.protobuf.ByteString.copyFrom(
                                new byte[] {(byte) 0x89, 'P', 'N', 'G'}))
                        .setImportance(4)
                        .setHighPriority(true)
                        .setClearable(true)
                        .setWhenMillis(1700000000000L))
                .build());
        cases.put("notification_removed", Envelope.newBuilder()
                .setNotificationRemoved(NotificationRemoved.newBuilder().setKey(chatKey))
                .build());
        cases.put("notification_clicked", Envelope.newBuilder()
                .setNotificationClicked(NotificationClicked.newBuilder().setKey(chatKey))
                .build());
        cases.put("notification_dismissed", Envelope.newBuilder()
                .setNotificationDismissed(NotificationDismissed.newBuilder().setKey(chatKey))
                .build());
        cases.put("agent_connected", Envelope.newBuilder()
                .setAgentConnected(AgentConnected.getDefaultInstance())
                .build());
        cases.put("install_apk", Envelope.newBuilder()
                .setInstallApk(InstallApk.newBuilder().setTransferId(3).setName("notes.apk").setSize(1000))
                .build());
        cases.put("apk_chunk", Envelope.newBuilder()
                .setApkChunk(ApkChunk.newBuilder().setTransferId(3).setOffset(131072)
                        .setData(com.google.protobuf.ByteString.copyFrom(new byte[] {1, 2, 3})))
                .build());
        cases.put("cancel_install", Envelope.newBuilder()
                .setCancelInstall(CancelInstall.newBuilder().setTransferId(3))
                .build());
        cases.put("install_progress", Envelope.newBuilder()
                .setInstallProgress(InstallProgress.newBuilder().setTransferId(3).setReceived(131072))
                .build());
        cases.put("install_result_success", Envelope.newBuilder()
                .setInstallResult(InstallResult.newBuilder().setTransferId(3)
                        .setStatus(InstallStatus.INSTALL_STATUS_SUCCESS).setPackageName("com.example.notes"))
                .build());
        cases.put("install_result_conflicting_signature", Envelope.newBuilder()
                .setInstallResult(InstallResult.newBuilder().setTransferId(3)
                        .setStatus(InstallStatus.INSTALL_STATUS_CONFLICTING_SIGNATURE)
                        .setPackageName("com.example.notes")
                        .setMessage("INSTALL_FAILED_UPDATE_INCOMPATIBLE"))
                .build());
        cases.put("guest_boot", Envelope.newBuilder()
                .setGuestBoot(GuestBoot.newBuilder().setBootCompleted(true).setAgentConnected(true))
                .build());
        cases.put("clock_probe", Envelope.newBuilder()
                .setClockProbe(ClockProbe.newBuilder().setHostSentNs(1_234_567_890_123L))
                .build());
        cases.put("clock_answer", Envelope.newBuilder()
                .setClockAnswer(ClockAnswer.newBuilder()
                        .setHostSentNs(1_234_567_890_123L)
                        .setGuestMonoNs(987_654_321_098L))
                .build());
        cases.put("hello_rejected_authentication", Envelope.newBuilder()
                .setHelloRejected(HelloRejected.newBuilder()
                        .setReason(RejectReason.REJECT_REASON_AUTHENTICATION_FAILED)
                        .setDetail("no proof"))
                .build());
        cases.put("request_guest_memory", Envelope.newBuilder()
                .setRequestGuestMemory(RequestGuestMemory.getDefaultInstance())
                .build());
        cases.put("guest_memory", Envelope.newBuilder()
                .setGuestMemory(GuestMemory.newBuilder()
                        .setMemTotalKb(4015356)
                        .setCachedKb(1503112)
                        .setZramUsedKb(30720)
                        .setTotalPssKb(330000)
                        .addTopProcesses(ProcessMemory.newBuilder()
                                .setPid(500)
                                .setName("system")
                                .setPssKb(180000)
                                .setSwapPssKb(1000)))
                .build());
        cases.put("open_link", Envelope.newBuilder()
                .setOpenLink(OpenLink.newBuilder().setUrl("https://example.com/"))
                .build());
        cases.put("link_handling", Envelope.newBuilder()
                .setLinkHandling(LinkHandling.newBuilder().setInGuest(true))
                .build());
        cases.put("open_file", Envelope.newBuilder()
                .setOpenFile(OpenFile.newBuilder()
                        .setTransferId(3)
                        .setName("photo.jpg")
                        .setSize(1000)
                        .setAction(FileAction.FILE_ACTION_SEND))
                .build());
        cases.put("file_chunk", Envelope.newBuilder()
                .setFileChunk(FileChunk.newBuilder()
                        .setTransferId(3)
                        .setOffset(131072)
                        .setData(ByteString.copyFrom(new byte[] {1, 2, 3})))
                .build());
        cases.put("file_progress", Envelope.newBuilder()
                .setFileProgress(FileProgress.newBuilder()
                        .setTransferId(3)
                        .setReceived(1000)
                        .setError("storage is full"))
                .build());
        cases.put("accessibility_watch_on", Envelope.newBuilder()
                .setAccessibilityWatch(AccessibilityWatch.newBuilder().setTaskId(42).setOn(true))
                .build());
        cases.put("accessibility_watch_off", Envelope.newBuilder()
                .setAccessibilityWatch(AccessibilityWatch.newBuilder().setTaskId(42))
                .build());
        cases.put("accessibility_tree_full", Envelope.newBuilder()
                .setAccessibilityTree(AccessibilityTree.newBuilder()
                        .setTaskId(42)
                        .setEpoch(1)
                        .setFull(true)
                        .addRootIds(1)
                        .addNodes(AccessibilityNode.newBuilder()
                                .setId(1)
                                .addChildIds(2)
                                .addChildIds(3)
                                .setClassName("android.widget.FrameLayout")
                                .setBounds(Rect.newBuilder().setLeft(0).setTop(0).setRight(400).setBottom(600))
                                .setEnabled(true))
                        .addNodes(AccessibilityNode.newBuilder()
                                .setId(2)
                                .setClassName("android.widget.TextView")
                                .setText("Network & internet")
                                .setBounds(Rect.newBuilder().setLeft(0).setTop(0).setRight(400).setBottom(80))
                                .setEnabled(true)
                                .setClickable(true)
                                .addActions(NodeAction.NODE_ACTION_CLICK)
                                .addActions(NodeAction.NODE_ACTION_ACCESSIBILITY_FOCUS))
                        .addNodes(AccessibilityNode.newBuilder()
                                .setId(3)
                                .setClassName("android.widget.SeekBar")
                                .setContentDescription("Volume")
                                .setBounds(Rect.newBuilder().setLeft(16).setTop(100).setRight(384).setBottom(140))
                                .setEnabled(true)
                                .setFocusable(true)
                                .setRange(NodeRange.newBuilder().setMin(0f).setMax(100f).setCurrent(50f))
                                .addActions(NodeAction.NODE_ACTION_SCROLL_FORWARD)
                                .addActions(NodeAction.NODE_ACTION_SCROLL_BACKWARD)
                                .setStateDescription("50 percent"))
                        .setFocusedId(3))
                .build());
        cases.put("accessibility_tree_delta", Envelope.newBuilder()
                .setAccessibilityTree(AccessibilityTree.newBuilder()
                        .setTaskId(42)
                        .setEpoch(1)
                        .addRootIds(1)
                        .addNodes(AccessibilityNode.newBuilder()
                                .setId(2)
                                .setClassName("android.widget.EditText")
                                .setText("Wi-Fi")
                                .setHint("Name")
                                .setViewId("com.android.settings:id/title")
                                .setBounds(Rect.newBuilder().setLeft(0).setTop(0).setRight(400).setBottom(80))
                                .setCheckable(true)
                                .setChecked(true)
                                .setSelected(true)
                                .setEnabled(true)
                                .setFocusable(true)
                                .setFocused(true)
                                .setScrollable(true)
                                .setClickable(true)
                                .setLongClickable(true)
                                .setEditable(true)
                                .setPassword(true)
                                .setHeading(true)
                                .addActions(NodeAction.NODE_ACTION_CLICK)
                                .addActions(NodeAction.NODE_ACTION_LONG_CLICK)
                                .addActions(NodeAction.NODE_ACTION_SET_TEXT)
                                .addActions(NodeAction.NODE_ACTION_FOCUS)
                                .addActions(NodeAction.NODE_ACTION_SELECT))
                        .addRemovedIds(3))
                .build());
        cases.put("accessibility_action", Envelope.newBuilder()
                .setAccessibilityAction(AccessibilityAction.newBuilder()
                        .setTaskId(42)
                        .setEpoch(1)
                        .setNodeId(2)
                        .setAction(NodeAction.NODE_ACTION_SET_TEXT)
                        .setText("hello")
                        .setRequestId(7))
                .build());
        cases.put("accessibility_action_result", Envelope.newBuilder()
                .setAccessibilityActionResult(AccessibilityActionResult.newBuilder()
                        .setTaskId(42)
                        .setRequestId(7)
                        .setDone(false)
                        .setError("the node is gone"))
                .build());
        WidgetProvider clock = WidgetProvider.newBuilder()
                .setProvider("com.example.clock/.ClockWidget")
                .setPackageName("com.example.clock")
                .setLabel("Clock")
                .setPreviewPng(ByteString.copyFrom(new byte[] {(byte) 0x89, 'P', 'N', 'G'}))
                .setMinWidthDp(110)
                .setMinHeightDp(40)
                .setDefaultWidthDp(180)
                .setDefaultHeightDp(110)
                .setResizeMode(3)
                .build();
        cases.put("list_widget_providers", Envelope.newBuilder()
                .setListWidgetProviders(ListWidgetProviders.newBuilder().setRequestId(7))
                .build());
        cases.put("widget_providers", Envelope.newBuilder()
                .setWidgetProviders(WidgetProviders.newBuilder()
                        .setRequestId(7)
                        .addProviders(clock)
                        .setComplete(true))
                .build());
        cases.put("add_widget", Envelope.newBuilder()
                .setAddWidget(AddWidget.newBuilder()
                        .setRequestId(8)
                        .setProvider("com.example.clock/.ClockWidget"))
                .build());
        cases.put("widget_added", Envelope.newBuilder()
                .setWidgetAdded(WidgetAdded.newBuilder().setRequestId(8).setWidgetId(12))
                .build());
        cases.put("widget_added_refused", Envelope.newBuilder()
                .setWidgetAdded(WidgetAdded.newBuilder()
                        .setRequestId(9)
                        .setError("the user canceled the widget's setup"))
                .build());
        cases.put("widget_updated", Envelope.newBuilder()
                .setWidgetUpdated(WidgetUpdated.newBuilder().setWidgetId(12).setProvider(clock))
                .build());
        cases.put("widget_list", Envelope.newBuilder()
                .setWidgetList(WidgetList.newBuilder().addWidgetIds(12).addWidgetIds(15))
                .build());
        cases.put("remove_widget", Envelope.newBuilder()
                .setRemoveWidget(RemoveWidget.newBuilder().setWidgetId(12))
                .build());
        cases.put("resize_widget", Envelope.newBuilder()
                .setResizeWidget(ResizeWidget.newBuilder()
                        .setWidgetId(12)
                        .setWidth(360)
                        .setHeight(220)
                        .setDensityDpi(240)
                        .setWidthDp(240)
                        .setHeightDp(146))
                .build());
        cases.put("widget_frame", Envelope.newBuilder()
                .setWidgetFrame(WidgetFrame.newBuilder()
                        .setWidgetId(12)
                        .setWidth(2)
                        .setHeight(1)
                        .setPng(ByteString.copyFrom(new byte[] {(byte) 0x89, 'P', 'N', 'G'})))
                .build());
        cases.put("widget_pointer", Envelope.newBuilder()
                .setWidgetPointer(WidgetPointer.newBuilder()
                        .setWidgetId(12)
                        .setAction(PointerAction.POINTER_ACTION_DOWN)
                        .setX(10.5f)
                        .setY(20.25f))
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
