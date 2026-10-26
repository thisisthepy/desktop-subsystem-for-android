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
    v1::HelloRejected* rejected = cases["hello_rejected_authentication"].mutable_hello_rejected();
    rejected->set_reason(v1::REJECT_REASON_AUTHENTICATION_FAILED);
    rejected->set_detail("no proof");
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
