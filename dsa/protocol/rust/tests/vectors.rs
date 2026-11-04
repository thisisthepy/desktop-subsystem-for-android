//! The checked-in protocol vectors, against the generated Rust codec.
//!
//! `dsa/protocol/vectors/control.vectors` holds, for each named case, the bytes of one
//! Envelope. The Java and C++ tests build the same cases and must produce the same bytes.
//! Set `DSA_WRITE_VECTORS=1` to rewrite the file from the cases below (then rerun every
//! language's tests).

use std::collections::BTreeMap;
use std::path::PathBuf;

use dsa_protocol::control::{
    envelope::Body, AccessibilityAction, AccessibilityActionResult, AccessibilityNode,
    AccessibilityTree, AccessibilityWatch, AgentConnected, ApkChunk, AppInfo, AuthChallenge,
    AuthProof, CancelInstall, ClipboardSharing, ClipboardText, ClockAnswer, ClockProbe, CloseTask,
    DeviceIdle, Envelope, FileAction, FileChunk, FileProgress, GuestBoot, GuestMemory,
    GuestNotification, Hello, HelloAccepted, HelloRejected, ImeCommit, ImeComposition, ImeCursor,
    InstallApk, InstallProgress, InstallResult, InstallStatus, KeyAction, KeyEvent, LaunchApp,
    LinkHandling, NodeAction, NodeRange, NotificationClicked, NotificationDismissed,
    NotificationRemoved, OpenFile, OpenLink, PackageState, PointerAction, PointerEvent,
    PointerTool, PowerState, ProcessMemory, QueryPackage, Rect, RejectReason, RequestGuestMemory,
    ResizeTask, Role, ScreenState, SetTaskDensity, TaskFrames, TaskRemoved, TaskUpdated,
};
use dsa_protocol::control::{
    AddWidget, ListWidgetProviders, RemoveWidget, ResizeWidget, WidgetAdded, WidgetFrame,
    WidgetList, WidgetPointer, WidgetProvider, WidgetProviders, WidgetUpdated,
};
use dsa_protocol::{decode, encode};

/// The key of the notification in the notification cases.
const CHAT_KEY: &str = "0|com.example.chat|1|null|10100";

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
/// The clock widget of the widget cases.
fn clock_provider() -> WidgetProvider {
    WidgetProvider {
        provider: "com.example.clock/.ClockWidget".into(),
        package_name: "com.example.clock".into(),
        label: "Clock".into(),
        preview_png: vec![0x89, b'P', b'N', b'G'],
        min_width_dp: 110,
        min_height_dp: 40,
        default_width_dp: 180,
        default_height_dp: 110,
        max_width_dp: 0,
        max_height_dp: 0,
        resize_mode: 3,
        needs_configuration: false,
    }
}

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
        (
            "auth_challenge",
            wrap(Body::AuthChallenge(AuthChallenge {
                nonce: (0x40u8..0x60).collect(),
            })),
        ),
        (
            "auth_proof",
            wrap(Body::AuthProof(AuthProof {
                mac: (0x60u8..0x80).collect(),
            })),
        ),
        (
            "task_updated",
            wrap(Body::TaskUpdated(TaskUpdated {
                task_id: 42,
                bounds: Some(Rect {
                    left: 100,
                    top: 80,
                    right: 1180,
                    bottom: 800,
                }),
                package_name: "com.example.notes".into(),
                label: "메모 (Notes)".into(),
                focused: true,
                ..TaskUpdated::default()
            })),
        ),
        (
            "task_updated_density",
            wrap(Body::TaskUpdated(TaskUpdated {
                task_id: 42,
                bounds: Some(Rect {
                    left: 0,
                    top: 0,
                    right: 1096,
                    bottom: 1948,
                }),
                package_name: "com.example.notes".into(),
                density_dpi: 320,
                max_width: 3840,
                max_height: 2160,
                ..TaskUpdated::default()
            })),
        ),
        (
            "resize_task",
            wrap(Body::ResizeTask(ResizeTask {
                task_id: 42,
                width: 1280,
                height: 720,
                live: false,
            })),
        ),
        (
            "set_task_density",
            wrap(Body::SetTaskDensity(SetTaskDensity {
                task_id: 42,
                density_dpi: 240,
            })),
        ),
        (
            "task_updated_negative_origin",
            wrap(Body::TaskUpdated(TaskUpdated {
                task_id: 7,
                bounds: Some(Rect {
                    left: -40,
                    top: -10,
                    right: 600,
                    bottom: 400,
                }),
                ..TaskUpdated::default()
            })),
        ),
        (
            "task_removed",
            wrap(Body::TaskRemoved(TaskRemoved { task_id: 42 })),
        ),
        (
            "task_frames",
            wrap(Body::TaskFrames(TaskFrames {
                task_id: 42,
                display_id: 3,
            })),
        ),
        (
            "close_task",
            wrap(Body::CloseTask(CloseTask { task_id: 42 })),
        ),
        (
            "launch_app",
            wrap(Body::LaunchApp(LaunchApp {
                package_name: "com.example.notes".into(),
            })),
        ),
        (
            "app_info",
            wrap(Body::AppInfo(AppInfo {
                package_name: "com.example.notes".into(),
                label: "Notes".into(),
                icon_png: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            })),
        ),
        (
            "pointer_event_mouse_secondary_down",
            wrap(Body::PointerEvent(PointerEvent {
                task_id: 42,
                action: PointerAction::Down as i32,
                tool: PointerTool::Mouse as i32,
                pointer_id: 0,
                x: 120.5,
                y: 64.25,
                buttons: 2,
                pressure: 1.0,
                ..PointerEvent::default()
            })),
        ),
        (
            "pointer_event_touch_move",
            wrap(Body::PointerEvent(PointerEvent {
                task_id: 7,
                action: PointerAction::Move as i32,
                tool: PointerTool::Touch as i32,
                pointer_id: 3,
                x: 10.0,
                y: 1023.75,
                pressure: 0.5,
                ..PointerEvent::default()
            })),
        ),
        (
            "pointer_event_scroll",
            wrap(Body::PointerEvent(PointerEvent {
                task_id: 42,
                action: PointerAction::Scroll as i32,
                tool: PointerTool::Mouse as i32,
                x: 300.0,
                y: 200.0,
                scroll_x: 0.5,
                scroll_y: -1.0,
                ..PointerEvent::default()
            })),
        ),
        (
            "pointer_event_pen_hover",
            wrap(Body::PointerEvent(PointerEvent {
                task_id: 42,
                action: PointerAction::HoverMove as i32,
                tool: PointerTool::Pen as i32,
                pointer_id: 1,
                x: 0.0,
                y: 599.0,
                buttons: 32,
                ..PointerEvent::default()
            })),
        ),
        (
            "key_event_ctrl_a_down",
            wrap(Body::KeyEvent(KeyEvent {
                task_id: 42,
                action: KeyAction::Down as i32,
                key_code: 29,
                meta_state: 0x3000,
                event_time_ms: 123_456,
                ..KeyEvent::default()
            })),
        ),
        (
            "key_event_backspace_repeat",
            wrap(Body::KeyEvent(KeyEvent {
                task_id: 42,
                action: KeyAction::Down as i32,
                key_code: 67,
                repeat_count: 3,
                event_time_ms: 500,
                ..KeyEvent::default()
            })),
        ),
        (
            "key_event_canceled_up",
            wrap(Body::KeyEvent(KeyEvent {
                task_id: 7,
                action: KeyAction::Up as i32,
                key_code: 66,
                meta_state: 0x10_0000,
                event_time_ms: 4_294_967_000,
                canceled: true,
                ..KeyEvent::default()
            })),
        ),
        (
            "ime_composition_korean",
            wrap(Body::ImeComposition(ImeComposition {
                task_id: 42,
                text: "\u{d55c}".into(),
                cursor: 1,
            })),
        ),
        (
            "ime_composition_removed",
            wrap(Body::ImeComposition(ImeComposition {
                task_id: 7,
                ..ImeComposition::default()
            })),
        ),
        (
            "ime_commit_korean",
            wrap(Body::ImeCommit(ImeCommit {
                task_id: 42,
                text: "\u{d55c}\u{ae00}".into(),
            })),
        ),
        (
            "ime_cursor",
            wrap(Body::ImeCursor(ImeCursor {
                task_id: 42,
                cursor: Some(Rect {
                    left: 100,
                    top: 200,
                    right: 102,
                    bottom: 240,
                }),
            })),
        ),
        (
            "screen_state_off",
            wrap(Body::ScreenState(ScreenState { on: false })),
        ),
        (
            "screen_state_on",
            wrap(Body::ScreenState(ScreenState { on: true })),
        ),
        (
            "power_state_on_battery",
            wrap(Body::PowerState(PowerState {
                ac_online: false,
                battery_present: true,
                battery_percent: 57,
                charging: false,
                power_saver: true,
            })),
        ),
        (
            "device_idle",
            wrap(Body::DeviceIdle(DeviceIdle { idle: true })),
        ),
        (
            "clipboard_text",
            wrap(Body::ClipboardText(ClipboardText {
                text: "hello \u{c548}\u{b155}".into(),
            })),
        ),
        (
            "clipboard_sharing_off",
            wrap(Body::ClipboardSharing(ClipboardSharing { on: false })),
        ),
        (
            "notification",
            wrap(Body::Notification(GuestNotification {
                key: CHAT_KEY.into(),
                package_name: "com.example.chat".into(),
                app_label: "Chat".into(),
                title: "Ann".into(),
                text: "\u{c548}\u{b155}".into(),
                icon_png: vec![0x89, b'P', b'N', b'G'],
                importance: 4,
                high_priority: true,
                clearable: true,
                when_millis: 1_700_000_000_000,
            })),
        ),
        (
            "notification_removed",
            wrap(Body::NotificationRemoved(NotificationRemoved {
                key: CHAT_KEY.into(),
            })),
        ),
        (
            "notification_clicked",
            wrap(Body::NotificationClicked(NotificationClicked {
                key: CHAT_KEY.into(),
            })),
        ),
        (
            "notification_dismissed",
            wrap(Body::NotificationDismissed(NotificationDismissed {
                key: CHAT_KEY.into(),
            })),
        ),
        (
            "install_apk",
            wrap(Body::InstallApk(InstallApk {
                transfer_id: 3,
                name: "notes.apk".into(),
                size: 1000,
            })),
        ),
        (
            "apk_chunk",
            wrap(Body::ApkChunk(ApkChunk {
                transfer_id: 3,
                offset: 131_072,
                data: vec![1, 2, 3],
            })),
        ),
        (
            "cancel_install",
            wrap(Body::CancelInstall(CancelInstall { transfer_id: 3 })),
        ),
        (
            "install_progress",
            wrap(Body::InstallProgress(InstallProgress {
                transfer_id: 3,
                received: 131_072,
            })),
        ),
        (
            "query_package",
            wrap(Body::QueryPackage(QueryPackage {
                request_id: 7,
                package_name: "com.example.notes".into(),
            })),
        ),
        (
            "package_state_installed",
            wrap(Body::PackageState(PackageState {
                request_id: 7,
                installed: true,
                version_code: 42,
                version_name: "1.2".into(),
                signer_sha256: vec![(0u8..32).collect()],
            })),
        ),
        (
            "install_result_success",
            wrap(Body::InstallResult(InstallResult {
                transfer_id: 3,
                status: InstallStatus::Success as i32,
                package_name: "com.example.notes".into(),
                message: String::new(),
            })),
        ),
        (
            "install_result_conflicting_signature",
            wrap(Body::InstallResult(InstallResult {
                transfer_id: 3,
                status: InstallStatus::ConflictingSignature as i32,
                package_name: "com.example.notes".into(),
                message: "INSTALL_FAILED_UPDATE_INCOMPATIBLE".into(),
            })),
        ),
        (
            "agent_connected",
            wrap(Body::AgentConnected(AgentConnected {})),
        ),
        (
            "guest_boot",
            wrap(Body::GuestBoot(GuestBoot {
                boot_completed: true,
                agent_connected: true,
            })),
        ),
        (
            "clock_probe",
            wrap(Body::ClockProbe(ClockProbe {
                host_sent_ns: 1_234_567_890_123,
            })),
        ),
        (
            "clock_answer",
            wrap(Body::ClockAnswer(ClockAnswer {
                host_sent_ns: 1_234_567_890_123,
                guest_mono_ns: 987_654_321_098,
            })),
        ),
        (
            "hello_rejected_authentication",
            wrap(Body::HelloRejected(HelloRejected {
                reason: RejectReason::AuthenticationFailed as i32,
                detail: "no proof".into(),
                host: None,
            })),
        ),
        (
            "request_guest_memory",
            wrap(Body::RequestGuestMemory(RequestGuestMemory {})),
        ),
        (
            "guest_memory",
            wrap(Body::GuestMemory(GuestMemory {
                mem_total_kb: 4015356,
                cached_kb: 1503112,
                zram_used_kb: 30720,
                total_pss_kb: 330000,
                top_processes: vec![ProcessMemory {
                    pid: 500,
                    name: "system".into(),
                    pss_kb: 180000,
                    swap_pss_kb: 1000,
                }],
                ..GuestMemory::default()
            })),
        ),
        (
            "open_link",
            wrap(Body::OpenLink(OpenLink {
                url: "https://example.com/".into(),
            })),
        ),
        (
            "link_handling",
            wrap(Body::LinkHandling(LinkHandling { in_guest: true })),
        ),
        (
            "open_file",
            wrap(Body::OpenFile(OpenFile {
                transfer_id: 3,
                name: "photo.jpg".into(),
                size: 1000,
                action: FileAction::Send as i32,
            })),
        ),
        (
            "file_chunk",
            wrap(Body::FileChunk(FileChunk {
                transfer_id: 3,
                offset: 131072,
                data: vec![1, 2, 3],
            })),
        ),
        (
            "file_progress",
            wrap(Body::FileProgress(FileProgress {
                transfer_id: 3,
                received: 1000,
                error: "storage is full".into(),
            })),
        ),
        (
            "list_widget_providers",
            wrap(Body::ListWidgetProviders(ListWidgetProviders {
                request_id: 7,
            })),
        ),
        (
            "widget_providers",
            wrap(Body::WidgetProviders(WidgetProviders {
                request_id: 7,
                providers: vec![clock_provider()],
                complete: true,
            })),
        ),
        (
            "add_widget",
            wrap(Body::AddWidget(AddWidget {
                request_id: 8,
                provider: "com.example.clock/.ClockWidget".into(),
            })),
        ),
        (
            "widget_added",
            wrap(Body::WidgetAdded(WidgetAdded {
                request_id: 8,
                widget_id: 12,
                error: String::new(),
            })),
        ),
        (
            "widget_added_refused",
            wrap(Body::WidgetAdded(WidgetAdded {
                request_id: 9,
                widget_id: 0,
                error: "the user canceled the widget's setup".into(),
            })),
        ),
        (
            "widget_updated",
            wrap(Body::WidgetUpdated(WidgetUpdated {
                widget_id: 12,
                provider: Some(clock_provider()),
            })),
        ),
        (
            "widget_list",
            wrap(Body::WidgetList(WidgetList {
                widget_ids: vec![12, 15],
            })),
        ),
        (
            "remove_widget",
            wrap(Body::RemoveWidget(RemoveWidget { widget_id: 12 })),
        ),
        (
            "resize_widget",
            wrap(Body::ResizeWidget(ResizeWidget {
                widget_id: 12,
                width: 360,
                height: 220,
                density_dpi: 240,
                width_dp: 240,
                height_dp: 146,
            })),
        ),
        (
            "widget_frame",
            wrap(Body::WidgetFrame(WidgetFrame {
                widget_id: 12,
                width: 2,
                height: 1,
                png: vec![0x89, b'P', b'N', b'G'],
            })),
        ),
        (
            "widget_pointer",
            wrap(Body::WidgetPointer(WidgetPointer {
                widget_id: 12,
                action: PointerAction::Down as i32,
                pointer_id: 0,
                x: 10.5,
                y: 20.25,
            })),
        ),
        (
            "accessibility_watch_on",
            wrap(Body::AccessibilityWatch(AccessibilityWatch {
                task_id: 42,
                on: true,
            })),
        ),
        (
            "accessibility_watch_off",
            wrap(Body::AccessibilityWatch(AccessibilityWatch {
                task_id: 42,
                on: false,
            })),
        ),
        (
            "accessibility_tree_full",
            wrap(Body::AccessibilityTree(AccessibilityTree {
                task_id: 42,
                epoch: 1,
                full: true,
                root_ids: vec![1],
                nodes: vec![
                    AccessibilityNode {
                        id: 1,
                        child_ids: vec![2, 3],
                        class_name: "android.widget.FrameLayout".into(),
                        bounds: Some(Rect {
                            left: 0,
                            top: 0,
                            right: 400,
                            bottom: 600,
                        }),
                        enabled: true,
                        ..AccessibilityNode::default()
                    },
                    AccessibilityNode {
                        id: 2,
                        class_name: "android.widget.TextView".into(),
                        text: "Network & internet".into(),
                        bounds: Some(Rect {
                            left: 0,
                            top: 0,
                            right: 400,
                            bottom: 80,
                        }),
                        enabled: true,
                        clickable: true,
                        actions: vec![
                            NodeAction::Click as i32,
                            NodeAction::AccessibilityFocus as i32,
                        ],
                        ..AccessibilityNode::default()
                    },
                    AccessibilityNode {
                        id: 3,
                        class_name: "android.widget.SeekBar".into(),
                        content_description: "Volume".into(),
                        bounds: Some(Rect {
                            left: 16,
                            top: 100,
                            right: 384,
                            bottom: 140,
                        }),
                        enabled: true,
                        focusable: true,
                        range: Some(NodeRange {
                            min: 0.0,
                            max: 100.0,
                            current: 50.0,
                        }),
                        actions: vec![
                            NodeAction::ScrollForward as i32,
                            NodeAction::ScrollBackward as i32,
                        ],
                        state_description: "50 percent".into(),
                        ..AccessibilityNode::default()
                    },
                ],
                removed_ids: vec![],
                focused_id: 3,
            })),
        ),
        (
            "accessibility_tree_delta",
            wrap(Body::AccessibilityTree(AccessibilityTree {
                task_id: 42,
                epoch: 1,
                full: false,
                root_ids: vec![1],
                nodes: vec![AccessibilityNode {
                    id: 2,
                    child_ids: vec![],
                    class_name: "android.widget.EditText".into(),
                    text: "Wi-Fi".into(),
                    content_description: String::new(),
                    hint: "Name".into(),
                    view_id: "com.android.settings:id/title".into(),
                    bounds: Some(Rect {
                        left: 0,
                        top: 0,
                        right: 400,
                        bottom: 80,
                    }),
                    checkable: true,
                    checked: true,
                    selected: true,
                    enabled: true,
                    focusable: true,
                    focused: true,
                    scrollable: true,
                    clickable: true,
                    long_clickable: true,
                    editable: true,
                    password: true,
                    heading: true,
                    range: None,
                    actions: vec![
                        NodeAction::Click as i32,
                        NodeAction::LongClick as i32,
                        NodeAction::SetText as i32,
                        NodeAction::Focus as i32,
                        NodeAction::Select as i32,
                    ],
                    state_description: String::new(),
                }],
                removed_ids: vec![3],
                focused_id: 0,
            })),
        ),
        (
            "accessibility_action",
            wrap(Body::AccessibilityAction(AccessibilityAction {
                task_id: 42,
                epoch: 1,
                node_id: 2,
                action: NodeAction::SetText as i32,
                text: "hello".into(),
                request_id: 7,
            })),
        ),
        (
            "accessibility_action_result",
            wrap(Body::AccessibilityActionResult(AccessibilityActionResult {
                task_id: 42,
                request_id: 7,
                done: false,
                error: "the node is gone".into(),
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
