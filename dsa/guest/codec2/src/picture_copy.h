// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Copies a picture the host decoded (NV12) into the planes of a guest graphics buffer, laid
// out however the buffer's allocator lays out YUV 4:2:0: separate or interleaved chroma, and
// any row and pixel stride.

#pragma once

#include <cstddef>
#include <cstdint>

namespace dsa::codec2 {

struct Plane {
    uint8_t* data = nullptr;
    // Bytes from one pixel to the next in a row, and from one row to the next.
    uint32_t pixelStride = 1;
    uint32_t rowStride = 0;
};

// The three planes of a YUV 4:2:0 buffer, as AHardwareBuffer_lockPlanes gives them.
struct YuvPlanes {
    Plane y;
    Plane u;
    Plane v;
};

// Copies the `width` by `height` NV12 picture in `nv12` (rows `stride` bytes long, the
// interleaved chroma after `stride * height` bytes) into `planes`, which must hold at least
// that size. Returns false, copying nothing, when `nv12` is too short for its size.
bool copyNv12(const uint8_t* nv12, size_t size, uint32_t width, uint32_t height, uint32_t stride,
              const YuvPlanes& planes);

}  // namespace dsa::codec2
