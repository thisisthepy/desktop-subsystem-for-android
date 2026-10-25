// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The control channel messages the hwcomposer reads and writes, through the codec
// generated from dsa/protocol/control.proto. Each frame is one encoded Envelope.

#pragma once

#include <cstddef>
#include <cstdint>
#include <variant>
#include <vector>

#include "task_layout.h"

namespace dsa::hwc {

// The agent reported a task, or its new bounds.
struct TaskUpdate {
    int32_t id = 0;
    Rect bounds;
};

// The agent reported that a task ended.
struct TaskEnd {
    int32_t id = 0;
};

// What a frame from the channel means to the hwcomposer. std::monostate for frames it has
// no use for (the host's messages, its own TaskFrames) and frames it cannot decode.
using ChannelEvent = std::variant<std::monostate, TaskUpdate, TaskEnd>;

ChannelEvent decodeChannelFrame(const uint8_t* data, size_t size);

// An Envelope carrying TaskFrames: the task's frames now arrive as posts to `displayId`.
std::vector<uint8_t> encodeTaskFrames(int32_t task, uint32_t displayId);

}  // namespace dsa::hwc
