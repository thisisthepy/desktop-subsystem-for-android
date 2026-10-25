// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "display.h"

#include <sys/system_properties.h>
#include <unistd.h>

#include <algorithm>
#include <cstdlib>
#include <cstring>

#include <cros_gralloc/cros_gralloc_handle.h>

#include "log.h"

namespace dsa::hwc {

namespace {

// minigbm writes this into every handle it makes (cros_gralloc_helpers.h).
constexpr uint32_t kCrosGrallocMagic = 0xABCDDCBA;

int32_t intProperty(const char* name, int32_t fallback, int32_t low, int32_t high) {
    char value[PROP_VALUE_MAX] = {};
    if (__system_property_get(name, value) <= 0) {
        return fallback;
    }
    char* end = nullptr;
    const long parsed = strtol(value, &end, 10);
    if (end == value || *end != '\0' || parsed < low || parsed > high) {
        HWC_LOGW("%s is %s, not a number from %d to %d; using %d", name, value, low, high, fallback);
        return fallback;
    }
    return static_cast<int32_t>(parsed);
}

}  // namespace

DisplayConfig DisplayConfig::fromProperties() {
    DisplayConfig config;
    config.width = intProperty("ro.boot.dsa.display_width", config.width, 320, 16384);
    config.height = intProperty("ro.boot.dsa.display_height", config.height, 320, 16384);
    config.dpi = intProperty("ro.boot.dsa.display_dpi", config.dpi, 72, 960);
    const int32_t hz = intProperty("ro.boot.dsa.display_refresh_rate", 60, 1, 240);
    config.vsyncPeriodNanos = 1000000000 / hz;
    return config;
}

BufferRef bufferRefFromHandle(const aidl::android::hardware::common::NativeHandle& handle) {
    // Rebuild the handle's words as gralloc laid them out: the descriptors, then the ints.
    // Only the ints are read, so the descriptors are left as -1.
    const size_t numFds = handle.fds.size();
    const size_t words = numFds + handle.ints.size();
    const size_t size = sizeof(native_handle_t) + words * sizeof(int);
    if (size < sizeof(cros_gralloc_handle)) {
        return {};
    }
    std::vector<uint8_t> storage(size);
    auto* native = reinterpret_cast<native_handle_t*>(storage.data());
    native->version = sizeof(native_handle_t);
    native->numFds = static_cast<int>(numFds);
    native->numInts = static_cast<int>(handle.ints.size());
    for (size_t i = 0; i < numFds; ++i) {
        native->data[i] = -1;
    }
    std::memcpy(&native->data[numFds], handle.ints.data(), handle.ints.size() * sizeof(int));
    const auto* cros = reinterpret_cast<const cros_gralloc_handle*>(native);
    if (cros->magic != kCrosGrallocMagic) {
        return {};
    }
    return BufferRef{cros->id, cros->width, cros->height};
}

BufferRef BufferSlots::apply(const composer3::Buffer& buffer) {
    if (buffer.handle) {
        const BufferRef ref = bufferRefFromHandle(*buffer.handle);
        mSlots[buffer.slot] = ref;
        return ref;
    }
    auto it = mSlots.find(buffer.slot);
    return it == mSlots.end() ? BufferRef{} : it->second;
}

void BufferSlots::collect(std::set<uint32_t>* ids) const {
    for (const auto& [_, ref] : mSlots) {
        if (ref.valid()) {
            ids->insert(ref.id);
        }
    }
}

int64_t Display::createLayer() {
    const int64_t id = mNextLayer++;
    mLayers[id].id = id;
    mValidated = false;
    return id;
}

bool Display::destroyLayer(int64_t id) {
    mValidated = false;
    return mLayers.erase(id) != 0;
}

Layer* Display::layer(int64_t id) {
    auto it = mLayers.find(id);
    return it == mLayers.end() ? nullptr : &it->second;
}

bool Display::applyLayerCommand(Layer& layer, const composer3::LayerCommand& c) {
    if (c.buffer) {
        layer.buffer = layer.slots.apply(*c.buffer);
        layer.acquireFence.reset(c.buffer->fence.get() >= 0 ? dup(c.buffer->fence.get()) : -1);
    }
    if (c.bufferSlotsToClear) {
        for (int32_t slot : *c.bufferSlotsToClear) {
            layer.slots.clear(slot);
        }
    }
    if (c.cursorPosition) {
        const int32_t width = layer.frame.right - layer.frame.left;
        const int32_t height = layer.frame.bottom - layer.frame.top;
        layer.frame = Rect{c.cursorPosition->x, c.cursorPosition->y,
                           c.cursorPosition->x + width, c.cursorPosition->y + height};
    }
    if (c.blendMode) {
        layer.blendMode = static_cast<int32_t>(c.blendMode->blendMode);
    }
    if (c.color) {
        layer.color[0] = c.color->r;
        layer.color[1] = c.color->g;
        layer.color[2] = c.color->b;
        layer.color[3] = c.color->a;
    }
    if (c.composition) {
        if (layer.composition != c.composition->composition) {
            mValidated = false;
        }
        layer.composition = c.composition->composition;
    }
    if (c.displayFrame) {
        layer.frame = Rect{c.displayFrame->left, c.displayFrame->top, c.displayFrame->right,
                           c.displayFrame->bottom};
    }
    if (c.planeAlpha) {
        layer.alpha = c.planeAlpha->alpha;
    }
    if (c.sourceCrop) {
        layer.crop = FRect{c.sourceCrop->left, c.sourceCrop->top, c.sourceCrop->right,
                           c.sourceCrop->bottom};
    }
    if (c.transform) {
        layer.transform = static_cast<int32_t>(c.transform->transform);
    }
    if (c.z) {
        layer.z = c.z->z;
    }
    if (c.luts) {
        layer.hasLuts = c.luts->pfd.get() >= 0;
    }
    return true;
}

void Display::setClientTarget(const composer3::Buffer& buffer) {
    mClientTarget = mClientSlots.apply(buffer);
    mClientTargetFence.reset(buffer.fence.get() >= 0 ? dup(buffer.fence.get()) : -1);
}

std::vector<composer3::ChangedCompositionLayer> Display::validate() {
    using composer3::Composition;
    mPendingChanges.clear();
    std::vector<composer3::ChangedCompositionLayer> changes;
    for (auto& [id, layer] : mLayers) {
        Composition wanted = layer.composition;
        switch (layer.composition) {
            case Composition::DEVICE:
            case Composition::SOLID_COLOR:
                if (layer.hasLuts) {
                    wanted = Composition::CLIENT;
                }
                break;
            case Composition::CURSOR:
                wanted = layer.hasLuts ? Composition::CLIENT : Composition::DEVICE;
                break;
            case Composition::SIDEBAND:
            case Composition::DISPLAY_DECORATION:
            case Composition::REFRESH_RATE_INDICATOR:
                wanted = Composition::CLIENT;
                break;
            case Composition::CLIENT:
            case Composition::INVALID:
            default:
                break;
        }
        if (wanted != layer.composition) {
            mPendingChanges[id] = wanted;
            composer3::ChangedCompositionLayer change;
            change.layer = id;
            change.composition = wanted;
            changes.push_back(change);
        }
    }
    mValidated = true;
    return changes;
}

void Display::acceptChanges() {
    for (const auto& [id, composition] : mPendingChanges) {
        if (Layer* l = layer(id)) {
            l->composition = composition;
        }
    }
    mPendingChanges.clear();
}

Frame Display::takeFrame() {
    using composer3::Composition;
    Frame frame;
    frame.display = bounds();
    for (auto& [id, layer] : mLayers) {
        layer.slots.collect(&frame.liveBuffers);
        if (layer.composition == Composition::INVALID) {
            continue;
        }
        FrameLayer out;
        out.id = id;
        out.z = layer.z;
        out.client = layer.composition == Composition::CLIENT;
        out.mode = layer.composition == Composition::SOLID_COLOR ? ComposeMode::SolidColor
                                                                 : ComposeMode::Device;
        out.buffer = layer.buffer;
        out.acquireFence = std::move(layer.acquireFence);
        out.frame = layer.frame;
        out.crop = layer.crop;
        out.blendMode = layer.blendMode;
        out.alpha = layer.alpha;
        for (int i = 0; i < 4; ++i) {
            out.color[i] = static_cast<uint8_t>(std::clamp(layer.color[i], 0.0f, 1.0f) * 255.0f + 0.5f);
        }
        out.transform = layer.transform;
        frame.layers.push_back(std::move(out));
    }
    mClientSlots.collect(&frame.liveBuffers);
    frame.clientTarget = mClientTarget;
    frame.clientTargetFence = std::move(mClientTargetFence);
    return frame;
}

}  // namespace dsa::hwc
