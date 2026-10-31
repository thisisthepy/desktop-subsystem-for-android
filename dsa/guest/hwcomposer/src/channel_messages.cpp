// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "channel_messages.h"

#include <string>

#include "control.pb.h"

namespace dsa::hwc {

namespace v1 = ::dsa::control::v1;

ChannelEvent decodeChannelFrame(const uint8_t* data, size_t size) {
    v1::Envelope envelope;
    if (!envelope.ParseFromArray(data, static_cast<int>(size))) {
        return std::monostate{};
    }
    switch (envelope.body_case()) {
        case v1::Envelope::kTaskUpdated: {
            const v1::TaskUpdated& updated = envelope.task_updated();
            const v1::Rect& b = updated.bounds();
            return TaskUpdate{updated.task_id(), Rect{b.left(), b.top(), b.right(), b.bottom()}};
        }
        case v1::Envelope::kTaskRemoved:
            return TaskEnd{envelope.task_removed().task_id()};
        default:
            return std::monostate{};
    }
}

std::vector<uint8_t> encodeTaskFrames(int32_t task, uint32_t displayId) {
    v1::Envelope envelope;
    v1::TaskFrames* frames = envelope.mutable_task_frames();
    frames->set_task_id(task);
    frames->set_display_id(displayId);
    const std::string bytes = envelope.SerializeAsString();
    return std::vector<uint8_t>(bytes.begin(), bytes.end());
}

}  // namespace dsa::hwc
