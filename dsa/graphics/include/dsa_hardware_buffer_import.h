// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Importing a gralloc buffer someone else allocated as an AHardwareBuffer, from its handle,
// in a process built against the NDK: AHardwareBuffer_createFromHandle is a vendor call, and
// the NDK's headers leave it and its methods out, so they are spelled here once for every
// such process (the hwcomposer, the camera HAL).
//
// The methods are the platform's numbers (vndk/hardware_buffer.h), which follow
// GraphicBuffer::HandleWrapMethod: 0 and 1 wrap or take a handle without importing it, and the
// call refuses both with BAD_VALUE (-22) before it reaches the mapper, so nothing is logged.
// The hwcomposer once passed 1 for "clone": every buffer a CPU wrote (a video decoder's
// pictures among them) failed to import, nothing was copied up to the host, and a video showed
// its sound over a green picture.
//
// The description must match the buffer as it was allocated: the mapper (Gralloc5) compares
// its width, height, requested format, layer count and stride with the handle's own, and
// refuses the import when one differs.

#pragma once

#include <cstdint>

namespace dsa {

// AHARDWAREBUFFER_CREATE_FROM_HANDLE_METHOD_REGISTER: the handle is taken over and imported.
inline constexpr int32_t kAhbCreateFromHandleRegister = 2;
// AHARDWAREBUFFER_CREATE_FROM_HANDLE_METHOD_CLONE: the handle is cloned and the clone
// imported; the caller keeps its own descriptors.
inline constexpr int32_t kAhbCreateFromHandleClone = 3;

// What a buffer's handle says about it, as minigbm writes it (cros_gralloc_handle).
struct GrallocBufferFacts {
    uint32_t width = 0;
    uint32_t height = 0;
    // The Android format the buffer was asked for (droid_format).
    int32_t androidFormat = 0;
    // The Android usage it was allocated with.
    uint64_t usage = 0;
    // The row length in pixels (pixel_stride), which the mapper reports as the stride.
    uint32_t pixelStride = 0;
};

// The AHardwareBuffer_Desc fields an import of that buffer passes, free of platform headers so
// the build machine tests them. `layers` is always 1: gralloc buffers here have one layer.
struct HardwareBufferImport {
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t layers = 1;
    uint32_t format = 0;
    uint64_t usage = 0;
    uint32_t stride = 0;
    int32_t method = kAhbCreateFromHandleClone;
};

// The import of a buffer whose handle says `facts`, cloning the handle.
inline HardwareBufferImport cloneImportFor(const GrallocBufferFacts& facts) {
    HardwareBufferImport import;
    import.width = facts.width;
    import.height = facts.height;
    import.layers = 1;
    import.format = static_cast<uint32_t>(facts.androidFormat);
    import.usage = facts.usage;
    import.stride = facts.pixelStride;
    import.method = kAhbCreateFromHandleClone;
    return import;
}

}  // namespace dsa
