// The checked-in protocol vectors, against the generated C++ (protobuf-lite) codec.
//
// The cases mirror dsa/protocol/rust/tests/vectors.rs exactly. Run with
// dsa/protocol/cpp/test.sh, which passes the protocol folder as the only argument.

#include <cstdio>
#include <fstream>
#include <map>
#include <sstream>
#include <string>
#include <vector>

#include "control.pb.h"
#include "control_schema.h"

namespace v1 = dsa::control::v1;

namespace {

std::vector<std::string> failures;

void Check(bool ok, const std::string& what) {
  if (!ok) failures.push_back(what);
}

std::string Bytes(int first, int count) {
  std::string bytes;
  for (int i = 0; i < count; ++i) bytes.push_back(static_cast<char>(first + i));
  return bytes;
}

v1::Hello GuestHello() {
  v1::Hello hello;
  hello.set_protocol_version(1);
  hello.set_schema_hash(Bytes(0x00, 32));
  hello.set_role(v1::ROLE_GUEST);
  hello.set_build("dsa-guestd 0.1.0");
  return hello;
}

v1::Hello HostHello() {
  v1::Hello hello;
  hello.set_protocol_version(1);
  hello.set_schema_hash(Bytes(0xa0, 32));
  hello.set_role(v1::ROLE_HOST);
  hello.set_build("dsa-host 0.1.0");
  return hello;
}

std::map<std::string, v1::Envelope> Cases() {
  std::map<std::string, v1::Envelope> cases;
  cases["envelope_empty"] = v1::Envelope();
  cases["hello_default"].mutable_hello();
  *cases["hello_guest"].mutable_hello() = GuestHello();
  {
    v1::Hello* hello = cases["hello_max_version"].mutable_hello();
    hello->set_protocol_version(0xffffffffu);
    hello->set_role(v1::ROLE_GUEST);
  }
  *cases["hello_accepted"].mutable_hello_accepted()->mutable_host() = HostHello();
  {
    v1::HelloRejected* rejected = cases["hello_rejected"].mutable_hello_rejected();
    rejected->set_reason(v1::REJECT_REASON_SCHEMA_HASH_MISMATCH);
    rejected->set_detail("스키마가 다릅니다 (schema differs)");
    *rejected->mutable_host() = HostHello();
  }
  cases["auth_challenge"].mutable_auth_challenge()->set_nonce(Bytes(0x40, 32));
  cases["auth_proof"].mutable_auth_proof()->set_mac(Bytes(0x60, 32));
  {
    v1::TaskUpdated* task = cases["task_updated"].mutable_task_updated();
    task->set_task_id(42);
    task->mutable_bounds()->set_left(100);
    task->mutable_bounds()->set_top(80);
    task->mutable_bounds()->set_right(1180);
    task->mutable_bounds()->set_bottom(800);
    task->set_package_name("com.example.notes");
    task->set_label("메모 (Notes)");
    task->set_focused(true);
  }
  {
    v1::TaskUpdated* task = cases["task_updated_density"].mutable_task_updated();
    task->set_task_id(42);
    task->mutable_bounds()->set_left(0);
    task->mutable_bounds()->set_top(0);
    task->mutable_bounds()->set_right(1096);
    task->mutable_bounds()->set_bottom(1948);
    task->set_package_name("com.example.notes");
    task->set_density_dpi(320);
    task->set_max_width(3840);
    task->set_max_height(2160);
  }
  {
    v1::ResizeTask* resize = cases["resize_task"].mutable_resize_task();
    resize->set_task_id(42);
    resize->set_width(1280);
    resize->set_height(720);
  }
  {
    v1::SetTaskDensity* density = cases["set_task_density"].mutable_set_task_density();
    density->set_task_id(42);
    density->set_density_dpi(240);
  }
  {
    v1::TaskUpdated* task = cases["task_updated_negative_origin"].mutable_task_updated();
    task->set_task_id(7);
    task->mutable_bounds()->set_left(-40);
    task->mutable_bounds()->set_top(-10);
    task->mutable_bounds()->set_right(600);
    task->mutable_bounds()->set_bottom(400);
  }
  cases["task_removed"].mutable_task_removed()->set_task_id(42);
  {
    v1::VideoCodecSupport* support = cases["video_capabilities"].mutable_video_capabilities()->add_codecs();
    support->set_codec(v1::VIDEO_CODEC_H264);
    support->set_max_width(4096);
    support->set_max_height(2304);
  }
  {
    v1::VideoDecoderOpen* open = cases["video_decoder_open"].mutable_video_decoder_open();
    open->set_codec(v1::VIDEO_CODEC_H264);
    open->set_width(1280);
    open->set_height(720);
  }
  cases["video_flush"].mutable_video_flush();
  {
    v1::VideoFrame* frame = cases["video_frame"].mutable_video_frame();
    frame->set_frame_index(3);
    frame->set_timestamp_us(100000);
    frame->set_width(2);
    frame->set_height(2);
    frame->set_stride(2);
    frame->set_nv12(std::string("\x10\x10\x10\x10\x80\x80", 6));
  }
  {
    v1::VideoPacket* packet = cases["video_packet"].mutable_video_packet();
    packet->set_frame_index(3);
    packet->set_timestamp_us(100000);
    packet->set_data(std::string("\x00\x00\x00\x01\x65", 5));
  }
  {
    v1::TaskFrames* frames = cases["task_frames"].mutable_task_frames();
    frames->set_task_id(42);
    frames->set_display_id(3);
  }
  cases["close_task"].mutable_close_task()->set_task_id(42);
  cases["launch_app"].mutable_launch_app()->set_package_name("com.example.notes");
  {
    v1::AppInfo* app = cases["app_info"].mutable_app_info();
    app->set_package_name("com.example.notes");
    app->set_label("Notes");
    app->set_icon_png(std::string("\x89PNG\r\n\x1a\n", 8));
  }
  {
    v1::PointerEvent* event = cases["pointer_event_mouse_secondary_down"].mutable_pointer_event();
    event->set_task_id(42);
    event->set_action(v1::POINTER_ACTION_DOWN);
    event->set_tool(v1::POINTER_TOOL_MOUSE);
    event->set_pointer_id(0);
    event->set_x(120.5f);
    event->set_y(64.25f);
    event->set_buttons(2);
    event->set_pressure(1.0f);
  }
  {
    v1::PointerEvent* event = cases["pointer_event_touch_move"].mutable_pointer_event();
    event->set_task_id(7);
    event->set_action(v1::POINTER_ACTION_MOVE);
    event->set_tool(v1::POINTER_TOOL_TOUCH);
    event->set_pointer_id(3);
    event->set_x(10.0f);
    event->set_y(1023.75f);
    event->set_pressure(0.5f);
  }
  {
    v1::PointerEvent* event = cases["pointer_event_scroll"].mutable_pointer_event();
    event->set_task_id(42);
    event->set_action(v1::POINTER_ACTION_SCROLL);
    event->set_tool(v1::POINTER_TOOL_MOUSE);
    event->set_x(300.0f);
    event->set_y(200.0f);
    event->set_scroll_x(0.5f);
    event->set_scroll_y(-1.0f);
  }
  {
    v1::PointerEvent* event = cases["pointer_event_pen_hover"].mutable_pointer_event();
    event->set_task_id(42);
    event->set_action(v1::POINTER_ACTION_HOVER_MOVE);
    event->set_tool(v1::POINTER_TOOL_PEN);
    event->set_pointer_id(1);
    event->set_x(0.0f);
    event->set_y(599.0f);
    event->set_buttons(32);
  }
  {
    v1::KeyEvent* event = cases["key_event_ctrl_a_down"].mutable_key_event();
    event->set_task_id(42);
    event->set_action(v1::KEY_ACTION_DOWN);
    event->set_key_code(29);
    event->set_meta_state(0x3000);
    event->set_event_time_ms(123456);
  }
  {
    v1::KeyEvent* event = cases["key_event_backspace_repeat"].mutable_key_event();
    event->set_task_id(42);
    event->set_action(v1::KEY_ACTION_DOWN);
    event->set_key_code(67);
    event->set_repeat_count(3);
    event->set_event_time_ms(500);
  }
  {
    v1::KeyEvent* event = cases["key_event_canceled_up"].mutable_key_event();
    event->set_task_id(7);
    event->set_action(v1::KEY_ACTION_UP);
    event->set_key_code(66);
    event->set_meta_state(0x100000);
    event->set_event_time_ms(4294967000u);
    event->set_canceled(true);
  }
  {
    v1::ImeComposition* composition = cases["ime_composition_korean"].mutable_ime_composition();
    composition->set_task_id(42);
    composition->set_text("\xed\x95\x9c");
    composition->set_cursor(1);
  }
  cases["ime_composition_removed"].mutable_ime_composition()->set_task_id(7);
  {
    v1::ImeCommit* commit = cases["ime_commit_korean"].mutable_ime_commit();
    commit->set_task_id(42);
    commit->set_text("\xed\x95\x9c\xea\xb8\x80");
  }
  {
    v1::ImeCursor* cursor = cases["ime_cursor"].mutable_ime_cursor();
    cursor->set_task_id(42);
    v1::Rect* rect = cursor->mutable_cursor();
    rect->set_left(100);
    rect->set_top(200);
    rect->set_right(102);
    rect->set_bottom(240);
  }
  cases["screen_state_off"].mutable_screen_state()->set_on(false);
  cases["screen_state_on"].mutable_screen_state()->set_on(true);
  {
    v1::PowerState* power = cases["power_state_on_battery"].mutable_power_state();
    power->set_ac_online(false);
    power->set_battery_present(true);
    power->set_battery_percent(57);
    power->set_charging(false);
    power->set_power_saver(true);
  }
  {
    v1::PowerHint* hint = cases["power_hint_launch_mode"].mutable_power_hint();
    hint->set_kind(v1::POWER_HINT_KIND_MODE);
    hint->set_id(5);
    hint->set_enabled(true);
  }
  {
    v1::PowerHint* hint = cases["power_hint_interaction_boost"].mutable_power_hint();
    hint->set_kind(v1::POWER_HINT_KIND_BOOST);
    hint->set_id(0);
    hint->set_duration_ms(80);
  }
  cases["device_idle"].mutable_device_idle()->set_idle(true);
  cases["clipboard_text"].mutable_clipboard_text()->set_text("hello \xec\x95\x88\xeb\x85\x95");
  cases["clipboard_sharing_off"].mutable_clipboard_sharing()->set_on(false);
  {
    const std::string key = "0|com.example.chat|1|null|10100";
    v1::GuestNotification* n = cases["notification"].mutable_notification();
    n->set_key(key);
    n->set_package_name("com.example.chat");
    n->set_app_label("Chat");
    n->set_title("Ann");
    n->set_text("\xec\x95\x88\xeb\x85\x95");
    n->set_icon_png(std::string("\x89PNG", 4));
    n->set_importance(4);
    n->set_high_priority(true);
    n->set_clearable(true);
    n->set_when_millis(1700000000000LL);
    cases["notification_removed"].mutable_notification_removed()->set_key(key);
    cases["notification_clicked"].mutable_notification_clicked()->set_key(key);
    cases["notification_dismissed"].mutable_notification_dismissed()->set_key(key);
  }
  cases["agent_connected"].mutable_agent_connected();
  {
    v1::InstallApk* install = cases["install_apk"].mutable_install_apk();
    install->set_transfer_id(3);
    install->set_name("notes.apk");
    install->set_size(1000);
  }
  {
    v1::ApkChunk* chunk = cases["apk_chunk"].mutable_apk_chunk();
    chunk->set_transfer_id(3);
    chunk->set_offset(131072);
    chunk->set_data(std::string("\x01\x02\x03", 3));
  }
  cases["cancel_install"].mutable_cancel_install()->set_transfer_id(3);
  {
    v1::InstallProgress* progress = cases["install_progress"].mutable_install_progress();
    progress->set_transfer_id(3);
    progress->set_received(131072);
  }
  {
    v1::QueryPackage* query = cases["query_package"].mutable_query_package();
    query->set_request_id(7);
    query->set_package_name("com.example.notes");
  }
  {
    v1::PackageState* state = cases["package_state_installed"].mutable_package_state();
    state->set_request_id(7);
    state->set_installed(true);
    state->set_version_code(42);
    state->set_version_name("1.2");
    state->add_signer_sha256(Bytes(0x00, 32));
  }
  {
    v1::InstallResult* result = cases["install_result_success"].mutable_install_result();
    result->set_transfer_id(3);
    result->set_status(v1::INSTALL_STATUS_SUCCESS);
    result->set_package_name("com.example.notes");
  }
  {
    v1::InstallResult* result =
        cases["install_result_conflicting_signature"].mutable_install_result();
    result->set_transfer_id(3);
    result->set_status(v1::INSTALL_STATUS_CONFLICTING_SIGNATURE);
    result->set_package_name("com.example.notes");
    result->set_message("INSTALL_FAILED_UPDATE_INCOMPATIBLE");
  }
  {
    v1::GuestBoot* boot = cases["guest_boot"].mutable_guest_boot();
    boot->set_boot_completed(true);
    boot->set_agent_connected(true);
  }
  cases["clock_probe"].mutable_clock_probe()->set_host_sent_ns(1234567890123ULL);
  {
    v1::ClockAnswer* answer = cases["clock_answer"].mutable_clock_answer();
    answer->set_host_sent_ns(1234567890123ULL);
    answer->set_guest_mono_ns(987654321098ULL);
  }
  {
    v1::HelloRejected* rejected = cases["hello_rejected_authentication"].mutable_hello_rejected();
    rejected->set_reason(v1::REJECT_REASON_AUTHENTICATION_FAILED);
    rejected->set_detail("no proof");
  }
  cases["request_guest_memory"].mutable_request_guest_memory();
  {
    v1::GuestMemory* memory = cases["guest_memory"].mutable_guest_memory();
    memory->set_mem_total_kb(4015356);
    memory->set_cached_kb(1503112);
    memory->set_zram_used_kb(30720);
    memory->set_total_pss_kb(330000);
    v1::ProcessMemory* process = memory->add_top_processes();
    process->set_pid(500);
    process->set_name("system");
    process->set_pss_kb(180000);
    process->set_swap_pss_kb(1000);
  }
  cases["open_link"].mutable_open_link()->set_url("https://example.com/");
  cases["link_handling"].mutable_link_handling()->set_in_guest(true);
  {
    v1::OpenFile* open = cases["open_file"].mutable_open_file();
    open->set_transfer_id(3);
    open->set_name("photo.jpg");
    open->set_size(1000);
    open->set_action(v1::FILE_ACTION_SEND);
  }
  {
    v1::FileChunk* chunk = cases["file_chunk"].mutable_file_chunk();
    chunk->set_transfer_id(3);
    chunk->set_offset(131072);
    chunk->set_data(std::string("\x01\x02\x03", 3));
  }
  {
    v1::FileProgress* progress = cases["file_progress"].mutable_file_progress();
    progress->set_transfer_id(3);
    progress->set_received(1000);
    progress->set_error("storage is full");
  }
  {
    v1::AccessibilityWatch* watch = cases["accessibility_watch_on"].mutable_accessibility_watch();
    watch->set_task_id(42);
    watch->set_on(true);
  }
  cases["accessibility_watch_off"].mutable_accessibility_watch()->set_task_id(42);
  {
    v1::AccessibilityTree* tree = cases["accessibility_tree_full"].mutable_accessibility_tree();
    tree->set_task_id(42);
    tree->set_epoch(1);
    tree->set_full(true);
    tree->add_root_ids(1);
    v1::AccessibilityNode* frame = tree->add_nodes();
    frame->set_id(1);
    frame->add_child_ids(2);
    frame->add_child_ids(3);
    frame->set_class_name("android.widget.FrameLayout");
    frame->mutable_bounds()->set_left(0);
    frame->mutable_bounds()->set_top(0);
    frame->mutable_bounds()->set_right(400);
    frame->mutable_bounds()->set_bottom(600);
    frame->set_enabled(true);
    v1::AccessibilityNode* item = tree->add_nodes();
    item->set_id(2);
    item->set_class_name("android.widget.TextView");
    item->set_text("Network & internet");
    item->mutable_bounds()->set_left(0);
    item->mutable_bounds()->set_top(0);
    item->mutable_bounds()->set_right(400);
    item->mutable_bounds()->set_bottom(80);
    item->set_enabled(true);
    item->set_clickable(true);
    item->add_actions(v1::NODE_ACTION_CLICK);
    item->add_actions(v1::NODE_ACTION_ACCESSIBILITY_FOCUS);
    v1::AccessibilityNode* slider = tree->add_nodes();
    slider->set_id(3);
    slider->set_class_name("android.widget.SeekBar");
    slider->set_content_description("Volume");
    slider->mutable_bounds()->set_left(16);
    slider->mutable_bounds()->set_top(100);
    slider->mutable_bounds()->set_right(384);
    slider->mutable_bounds()->set_bottom(140);
    slider->set_enabled(true);
    slider->set_focusable(true);
    slider->mutable_range()->set_min(0.0f);
    slider->mutable_range()->set_max(100.0f);
    slider->mutable_range()->set_current(50.0f);
    slider->add_actions(v1::NODE_ACTION_SCROLL_FORWARD);
    slider->add_actions(v1::NODE_ACTION_SCROLL_BACKWARD);
    slider->set_state_description("50 percent");
    tree->set_focused_id(3);
  }
  {
    v1::AccessibilityTree* tree = cases["accessibility_tree_delta"].mutable_accessibility_tree();
    tree->set_task_id(42);
    tree->set_epoch(1);
    tree->add_root_ids(1);
    v1::AccessibilityNode* node = tree->add_nodes();
    node->set_id(2);
    node->set_class_name("android.widget.EditText");
    node->set_text("Wi-Fi");
    node->set_hint("Name");
    node->set_view_id("com.android.settings:id/title");
    node->mutable_bounds()->set_left(0);
    node->mutable_bounds()->set_top(0);
    node->mutable_bounds()->set_right(400);
    node->mutable_bounds()->set_bottom(80);
    node->set_checkable(true);
    node->set_checked(true);
    node->set_selected(true);
    node->set_enabled(true);
    node->set_focusable(true);
    node->set_focused(true);
    node->set_scrollable(true);
    node->set_clickable(true);
    node->set_long_clickable(true);
    node->set_editable(true);
    node->set_password(true);
    node->set_heading(true);
    node->add_actions(v1::NODE_ACTION_CLICK);
    node->add_actions(v1::NODE_ACTION_LONG_CLICK);
    node->add_actions(v1::NODE_ACTION_SET_TEXT);
    node->add_actions(v1::NODE_ACTION_FOCUS);
    node->add_actions(v1::NODE_ACTION_SELECT);
    tree->add_removed_ids(3);
  }
  {
    v1::AccessibilityAction* action = cases["accessibility_action"].mutable_accessibility_action();
    action->set_task_id(42);
    action->set_epoch(1);
    action->set_node_id(2);
    action->set_action(v1::NODE_ACTION_SET_TEXT);
    action->set_text("hello");
    action->set_request_id(7);
  }
  {
    v1::AccessibilityActionResult* result =
        cases["accessibility_action_result"].mutable_accessibility_action_result();
    result->set_task_id(42);
    result->set_request_id(7);
    result->set_done(false);
    result->set_error("the node is gone");
  }
  {
    v1::LogStream* stream = cases["log_stream_on"].mutable_log_stream();
    stream->set_on(true);
    stream->set_buffers(50);
    stream->set_min_priority(4);
    stream->set_tail(500);
  }
  cases["log_stream_off"].mutable_log_stream();
  {
    v1::LogDump* dump = cases["log_dump"].mutable_log_dump();
    dump->set_request_id(7);
    dump->set_buffers(50);
    dump->set_min_priority(2);
  }
  {
    v1::LogRecords* records = cases["log_records"].mutable_log_records();
    v1::LogRecord* record = records->add_records();
    record->set_buffer(v1::LOG_BUFFER_MAIN);
    record->set_priority(4);
    record->set_time_ns(1700000000123456789ULL);
    record->set_pid(1234);
    record->set_tid(1240);
    record->set_uid(1000);
    record->set_tag("ActivityManager");
    record->set_message("Start proc 1234");
    records->add_records()->set_dropped(12);
  }
  {
    v1::LogRecords* end = cases["log_records_dump_end"].mutable_log_records();
    end->set_request_id(7);
    end->set_end(true);
  }
  cases["collect_diagnostics"].mutable_collect_diagnostics()->set_request_id(3);
  {
    v1::DiagnosticsFile* file = cases["diagnostics_file"].mutable_diagnostics_file();
    file->set_request_id(3);
    file->set_name("android/pstore/console-ramoops-0");
    file->set_data("boot");
    file->set_last(true);
  }
  {
    v1::DiagnosticsDone* done = cases["diagnostics_done"].mutable_diagnostics_done();
    done->set_request_id(3);
    done->set_error("the agent did not answer");
  }
  {
    const std::string provider = "com.example.clock/.ClockWidget";
    auto clock = [&](v1::WidgetProvider* p) {
      p->set_provider(provider);
      p->set_package_name("com.example.clock");
      p->set_label("Clock");
      p->set_preview_png(std::string("\x89PNG", 4));
      p->set_min_width_dp(110);
      p->set_min_height_dp(40);
      p->set_default_width_dp(180);
      p->set_default_height_dp(110);
      p->set_resize_mode(3);
    };
    cases["list_widget_providers"].mutable_list_widget_providers()->set_request_id(7);
    v1::WidgetProviders* providers = cases["widget_providers"].mutable_widget_providers();
    providers->set_request_id(7);
    clock(providers->add_providers());
    providers->set_complete(true);
    v1::AddWidget* add = cases["add_widget"].mutable_add_widget();
    add->set_request_id(8);
    add->set_provider(provider);
    v1::WidgetAdded* added = cases["widget_added"].mutable_widget_added();
    added->set_request_id(8);
    added->set_widget_id(12);
    v1::WidgetAdded* refused = cases["widget_added_refused"].mutable_widget_added();
    refused->set_request_id(9);
    refused->set_error("the user canceled the widget's setup");
    v1::WidgetUpdated* updated = cases["widget_updated"].mutable_widget_updated();
    updated->set_widget_id(12);
    clock(updated->mutable_provider());
    v1::WidgetList* list = cases["widget_list"].mutable_widget_list();
    list->add_widget_ids(12);
    list->add_widget_ids(15);
    cases["remove_widget"].mutable_remove_widget()->set_widget_id(12);
    v1::ResizeWidget* resize = cases["resize_widget"].mutable_resize_widget();
    resize->set_widget_id(12);
    resize->set_width(360);
    resize->set_height(220);
    resize->set_density_dpi(240);
    resize->set_width_dp(240);
    resize->set_height_dp(146);
    v1::WidgetFrame* frame = cases["widget_frame"].mutable_widget_frame();
    frame->set_widget_id(12);
    frame->set_width(2);
    frame->set_height(1);
    frame->set_png(std::string("\x89PNG", 4));
    v1::WidgetPointer* pointer = cases["widget_pointer"].mutable_widget_pointer();
    pointer->set_widget_id(12);
    pointer->set_action(v1::POINTER_ACTION_DOWN);
    pointer->set_x(10.5f);
    pointer->set_y(20.25f);
  }
  return cases;
}

std::string FromHex(const std::string& hex) {
  std::string bytes;
  for (size_t i = 0; i + 1 < hex.size(); i += 2) {
    bytes.push_back(static_cast<char>(std::stoi(hex.substr(i, 2), nullptr, 16)));
  }
  return bytes;
}

std::string ToHex(const std::string& bytes) {
  static const char* digits = "0123456789abcdef";
  std::string hex;
  for (unsigned char b : bytes) {
    hex.push_back(digits[b >> 4]);
    hex.push_back(digits[b & 0xf]);
  }
  return hex;
}

std::map<std::string, std::string> ReadVectors(const std::string& path) {
  std::map<std::string, std::string> vectors;
  std::ifstream file(path);
  std::string line;
  while (std::getline(file, line)) {
    if (line.empty() || line[0] == '#') continue;
    std::istringstream fields(line);
    std::string name, hex = "-";
    fields >> name >> hex;
    vectors[name] = hex == "-" ? std::string() : FromHex(hex);
  }
  return vectors;
}

void pr2_the_cpp_codec_encodes_every_vector_byte_for_byte(
    const std::map<std::string, std::string>& vectors) {
  auto cases = Cases();
  Check(vectors.size() == cases.size(), "the vector file and the cases list different names");
  for (const auto& [name, envelope] : cases) {
    auto expected = vectors.find(name);
    std::string actual = envelope.SerializeAsString();
    Check(expected != vectors.end() && expected->second == actual,
          "encoding " + name + ": got " + ToHex(actual) + ", expected " +
              (expected == vectors.end() ? std::string("nothing") : ToHex(expected->second)));
  }
}

void pr2_the_cpp_codec_decodes_every_vector_to_the_same_message(
    const std::map<std::string, std::string>& vectors) {
  for (const auto& [name, envelope] : Cases()) {
    auto bytes = vectors.find(name);
    if (bytes == vectors.end()) continue;
    v1::Envelope decoded;
    Check(decoded.ParseFromString(bytes->second), "parsing " + name);
    Check(decoded.body_case() == envelope.body_case(), "decoding " + name + ": wrong body");
    Check(decoded.SerializeAsString() == envelope.SerializeAsString(), "decoding " + name);
  }
}

void pr2_the_cpp_code_carries_the_shared_schema_hash(const std::string& protocol) {
  std::ifstream file(protocol + "/schema.sha256");
  std::string shared;
  file >> shared;
  Check(shared == v1::kSchemaHashHex,
        std::string("kSchemaHashHex is ") + v1::kSchemaHashHex + " but schema.sha256 is " +
            shared + "; regenerate with dsa-protocol-codegen");
}

}  // namespace

int main(int argc, char** argv) {
  if (argc != 2) {
    std::fprintf(stderr, "usage: vectors_test <dsa/protocol folder>\n");
    return 2;
  }
  std::string protocol = argv[1];
  auto vectors = ReadVectors(protocol + "/vectors/control.vectors");
  pr2_the_cpp_codec_encodes_every_vector_byte_for_byte(vectors);
  pr2_the_cpp_codec_decodes_every_vector_to_the_same_message(vectors);
  pr2_the_cpp_code_carries_the_shared_schema_hash(protocol);
  for (const auto& failure : failures) std::fprintf(stderr, "FAILED: %s\n", failure.c_str());
  if (!failures.empty()) return 1;
  std::printf("c++ vectors: %zu cases passed\n", Cases().size());
  return 0;
}
