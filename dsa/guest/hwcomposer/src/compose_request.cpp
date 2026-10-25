// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "compose_request.h"

#include <cstring>

namespace dsa::hwc {

std::vector<uint8_t> buildComposeRequest(uint32_t displayId, uint32_t target, const Rect& bounds,
                                         const std::vector<ComposeInput>& layers) {
    std::vector<uint8_t> out(sizeof(WireComposeDeviceV2) + layers.size() * sizeof(WireComposeLayer));
    WireComposeDeviceV2 header{2, displayId, target, static_cast<uint32_t>(layers.size())};
    std::memcpy(out.data(), &header, sizeof(header));

    uint8_t* cursor = out.data() + sizeof(header);
    for (const ComposeInput& in : layers) {
        const Rect frame = in.frame.relativeTo(bounds);
        WireComposeLayer layer{};
        layer.cbHandle = in.mode == ComposeMode::Device ? in.colorBuffer : 0;
        layer.composeMode = static_cast<int32_t>(in.mode);
        layer.displayFrame = WireRect{frame.left, frame.top, frame.right, frame.bottom};
        layer.crop = WireFRect{in.crop.left, in.crop.top, in.crop.right, in.crop.bottom};
        layer.blendMode = in.blendMode;
        layer.alpha = in.alpha;
        layer.color = WireColor{in.color[0], in.color[1], in.color[2], in.color[3]};
        layer.transform = in.transform;
        std::memcpy(cursor, &layer, sizeof(layer));
        cursor += sizeof(layer);
    }
    return out;
}

}  // namespace dsa::hwc
