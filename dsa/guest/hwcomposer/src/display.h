// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The one display SurfaceFlinger sees: the freeform desktop every task window lives on. It
// keeps the layers and their latest state; composition happens per task in TaskComposer.

#pragma once

#include <cstdint>
#include <map>
#include <optional>
#include <set>
#include <vector>

#include <aidl/android/hardware/common/NativeHandle.h>
#include <aidl/android/hardware/graphics/composer3/Buffer.h>
#include <aidl/android/hardware/graphics/composer3/ChangedCompositionLayer.h>
#include <aidl/android/hardware/graphics/composer3/Composition.h>
#include <aidl/android/hardware/graphics/composer3/LayerCommand.h>
#include <aidl/android/hardware/graphics/composer3/PowerMode.h>

#include "host_renderer.h"
#include "task_composer.h"
#include "unique_fd.h"

namespace dsa::hwc {

namespace composer3 = aidl::android::hardware::graphics::composer3;

// The display's one configuration, from the kernel command line (see README.md).
struct DisplayConfig {
    int32_t width = 1920;
    int32_t height = 1080;
    int32_t dpi = 160;
    int32_t vsyncPeriodNanos = 16666666;

    static DisplayConfig fromProperties();
};

// What a gralloc handle from SurfaceFlinger says about its buffer. Only minigbm handles
// are understood; any other gives an invalid BufferRef.
BufferRef bufferRefFromHandle(const aidl::android::hardware::common::NativeHandle& handle);

// The buffers SurfaceFlinger has put in a layer's (or the client target's) slots: it sends
// a buffer once, then names it by slot.
class BufferSlots {
   public:
    // The buffer a Buffer command names, remembering it in its slot if it carries a handle.
    BufferRef apply(const composer3::Buffer& buffer);
    void clear(int32_t slot) { mSlots.erase(slot); }
    void collect(std::set<uint32_t>* ids) const;

   private:
    std::map<int32_t, BufferRef> mSlots;
};

struct Layer {
    int64_t id = 0;
    composer3::Composition composition = composer3::Composition::INVALID;
    BufferSlots slots;
    BufferRef buffer;
    UniqueFd acquireFence;
    Rect frame;
    FRect crop;
    int32_t blendMode = 1;
    float alpha = 1.0f;
    float color[4] = {0, 0, 0, 1};
    int32_t transform = 0;
    int32_t z = 0;
    bool hasLuts = false;
};

class Display {
   public:
    explicit Display(const DisplayConfig& config) : mConfig(config) {}

    const DisplayConfig& config() const { return mConfig; }
    Rect bounds() const { return Rect{0, 0, mConfig.width, mConfig.height}; }

    int64_t createLayer();
    bool destroyLayer(int64_t id);
    Layer* layer(int64_t id);

    // Applies one layer command. Returns false on a bad parameter.
    bool applyLayerCommand(Layer& layer, const composer3::LayerCommand& command);
    void setClientTarget(const composer3::Buffer& buffer);

    // Decides each layer's composition: the host composes buffers and solid colours; what
    // it cannot (sideband streams, lookup tables) SurfaceFlinger composes as client layers.
    std::vector<composer3::ChangedCompositionLayer> validate();
    void acceptChanges();
    bool validated() const { return mValidated; }
    void invalidate() { mValidated = false; }

    // The frame to compose now. Takes the layers' acquire fences.
    Frame takeFrame();

    composer3::PowerMode powerMode() const { return mPowerMode; }
    void setPowerMode(composer3::PowerMode mode) { mPowerMode = mode; }

   private:
    DisplayConfig mConfig;
    std::map<int64_t, Layer> mLayers;
    int64_t mNextLayer = 1;
    BufferSlots mClientSlots;
    BufferRef mClientTarget;
    UniqueFd mClientTargetFence;
    std::map<int64_t, composer3::Composition> mPendingChanges;
    bool mValidated = false;
    composer3::PowerMode mPowerMode = composer3::PowerMode::OFF;
};

}  // namespace dsa::hwc
