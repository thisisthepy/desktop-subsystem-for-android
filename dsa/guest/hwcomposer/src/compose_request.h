// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The request the hwcomposer sends with renderControl's rcCompose: the host composes the
// listed layers into one ColorBuffer. The layout is the host renderer's ComposeDevice_v2
// and ComposeLayer (gfxstream host/Hwc2.h), byte for byte.

#pragma once

#include <cstdint>
#include <vector>

#include "task_layout.h"

namespace dsa::hwc {

// hwc2_composition_t values the host compositor understands.
enum class ComposeMode : int32_t {
    Device = 2,
    SolidColor = 3,
};

struct FRect {
    float left = 0;
    float top = 0;
    float right = 0;
    float bottom = 0;
};

// One layer as the composer knows it, in display coordinates.
struct ComposeInput {
    ComposeMode mode = ComposeMode::Device;
    // The host ColorBuffer the layer shows; 0 for a solid colour.
    uint32_t colorBuffer = 0;
    Rect frame;
    FRect crop;
    // The Android blend mode (NONE 1, PREMULTIPLIED 2, COVERAGE 3), which the host takes as is.
    int32_t blendMode = 1;
    float alpha = 1.0f;
    uint8_t color[4] = {0, 0, 0, 0};
    // The Android transform bits, which the host takes as is.
    int32_t transform = 0;
};

#pragma pack(push, 1)
struct WireRect {
    int32_t left, top, right, bottom;
};
struct WireFRect {
    float left, top, right, bottom;
};
struct WireColor {
    uint8_t r, g, b, a;
};
struct WireComposeLayer {
    uint32_t cbHandle;
    int32_t composeMode;
    WireRect displayFrame;
    WireFRect crop;
    int32_t blendMode;
    float alpha;
    WireColor color;
    int32_t transform;
};
struct WireComposeDeviceV2 {
    uint32_t version;
    uint32_t displayId;
    uint32_t targetHandle;
    uint32_t numLayers;
};
#pragma pack(pop)

static_assert(sizeof(WireComposeLayer) == 56, "ComposeLayer must match the host's layout");
static_assert(sizeof(WireComposeDeviceV2) == 16, "ComposeDevice_v2 must match the host's layout");

// A ComposeDevice_v2 that composes `layers` (bottom to top) into `target`, a ColorBuffer at
// least the size of `bounds`, naming host display `displayId` (0 to name none). Each layer's
// frame is moved so that the task's top left corner is the target's origin, so the frame
// fills the target's top-left `bounds`-sized region; crops stay in buffer coordinates.
std::vector<uint8_t> buildComposeRequest(uint32_t displayId, uint32_t target, const Rect& bounds,
                                         const std::vector<ComposeInput>& layers);

}  // namespace dsa::hwc
