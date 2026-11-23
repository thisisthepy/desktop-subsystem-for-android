// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The hwcomposer's channel messages against the checked-in protocol vectors, so its reading
// of TaskUpdated and TaskRemoved and its TaskFrames stay in step with the Rust and Kotlin
// sides. The protocol folder is the only argument.

#include <fstream>
#include <map>
#include <sstream>
#include <string>
#include <vector>

#include "channel_messages.h"
#include "test_main.h"

using namespace dsa::hwc;

namespace {

std::string gProtocolDir;

std::vector<uint8_t> vector(const std::string& name) {
    std::ifstream in(gProtocolDir + "/vectors/control.vectors");
    std::string line;
    while (std::getline(in, line)) {
        std::istringstream fields(line);
        std::string key, hex;
        fields >> key >> hex;
        if (key != name) {
            continue;
        }
        std::vector<uint8_t> bytes;
        for (size_t i = 0; hex != "-" && i + 1 < hex.size(); i += 2) {
            bytes.push_back(static_cast<uint8_t>(std::stoi(hex.substr(i, 2), nullptr, 16)));
        }
        return bytes;
    }
    ::dsa::test::fail(__FILE__, __LINE__, "the vectors have no case " + name);
    return {};
}

}  // namespace

TEST(fr3_task_frames_encode_as_the_protocol_vector) {
    EXPECT_EQ(encodeTaskFrames(42, 3), vector("task_frames"));
}

TEST(fr3_task_updated_gives_the_task_and_its_bounds) {
    const std::vector<uint8_t> bytes = vector("task_updated");
    const ChannelEvent event = decodeChannelFrame(bytes.data(), bytes.size());
    const TaskUpdate* update = std::get_if<TaskUpdate>(&event);
    EXPECT_TRUE(update != nullptr);
    if (update) {
        EXPECT_EQ(update->id, 42);
        EXPECT_TRUE(update->bounds == (Rect{100, 80, 1180, 800}));
    }
}

TEST(fr3_task_bounds_may_start_left_of_and_above_the_display) {
    const std::vector<uint8_t> bytes = vector("task_updated_negative_origin");
    const ChannelEvent event = decodeChannelFrame(bytes.data(), bytes.size());
    const TaskUpdate* update = std::get_if<TaskUpdate>(&event);
    EXPECT_TRUE(update != nullptr);
    if (update) {
        EXPECT_EQ(update->id, 7);
        EXPECT_TRUE(update->bounds == (Rect{-40, -10, 600, 400}));
    }
}

TEST(fr41_task_updated_says_when_it_is_an_app_overlay_window) {
    const std::vector<uint8_t> bytes = vector("task_updated_overlay");
    const ChannelEvent event = decodeChannelFrame(bytes.data(), bytes.size());
    const TaskUpdate* update = std::get_if<TaskUpdate>(&event);
    EXPECT_TRUE(update != nullptr);
    if (update) {
        EXPECT_EQ(update->id, 0x40000001);
        EXPECT_TRUE(update->bounds == (Rect{158, 82, 666, 220}));
        EXPECT_TRUE(update->overlay);
    }
    // An app task is none.
    const std::vector<uint8_t> task = vector("task_updated");
    const ChannelEvent plain = decodeChannelFrame(task.data(), task.size());
    const TaskUpdate* taskUpdate = std::get_if<TaskUpdate>(&plain);
    EXPECT_TRUE(taskUpdate != nullptr && !taskUpdate->overlay);
}

TEST(fr3_task_removed_ends_the_task) {
    const std::vector<uint8_t> bytes = vector("task_removed");
    const ChannelEvent event = decodeChannelFrame(bytes.data(), bytes.size());
    const TaskEnd* end = std::get_if<TaskEnd>(&event);
    EXPECT_TRUE(end != nullptr);
    if (end) {
        EXPECT_EQ(end->id, 42);
    }
}

TEST(fr3_request_task_frame_asks_for_the_tasks_frame) {
    const std::vector<uint8_t> bytes = vector("request_task_frame");
    const ChannelEvent event = decodeChannelFrame(bytes.data(), bytes.size());
    const FrameRequest* request = std::get_if<FrameRequest>(&event);
    EXPECT_TRUE(request != nullptr);
    if (request) {
        EXPECT_EQ(request->id, 42);
    }
}

TEST(fr4_display_size_asks_for_the_display_to_grow) {
    const std::vector<uint8_t> bytes = vector("display_size");
    const ChannelEvent event = decodeChannelFrame(bytes.data(), bytes.size());
    const DisplayGrowth* growth = std::get_if<DisplayGrowth>(&event);
    EXPECT_TRUE(growth != nullptr);
    if (growth) {
        EXPECT_EQ(growth->width, 4096);
        EXPECT_EQ(growth->height, 4096);
    }
}

TEST(fr3_other_messages_mean_nothing_to_the_hwcomposer) {
    for (const char* name : {"close_task", "launch_app", "task_frames", "hello_guest",
                             "pointer_event_mouse_secondary_down", "resize_task",
                             "set_task_density"}) {
        const std::vector<uint8_t> bytes = vector(name);
        EXPECT_TRUE(std::holds_alternative<std::monostate>(decodeChannelFrame(bytes.data(), bytes.size())));
    }
}

TEST(nfr5_a_frame_that_does_not_decode_is_dropped) {
    const uint8_t garbage[] = {0xff, 0xff, 0xff, 0xff, 0x0f};
    EXPECT_TRUE(std::holds_alternative<std::monostate>(decodeChannelFrame(garbage, sizeof(garbage))));
}

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s <dsa/protocol folder>\n", argv[0]);
        return 2;
    }
    gProtocolDir = argv[1];
    return ::dsa::test::runAll();
}
