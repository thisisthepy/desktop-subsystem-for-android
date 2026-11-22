// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Fills an app's camera buffer from the host's picture (NV12), scaled to the buffer's size by
// taking the nearest pixel: each stream of a session may have a size of its own, while the
// host sends one picture, of the largest. YUV buffers take the planes however the buffer's
// allocator lays them out; others take RGBA.

#pragma once

#include <cstddef>
#include <cstdint>

namespace dsa::camera {

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

// The host's picture: `width` by `height` NV12, rows `stride` bytes long, the interleaved
// chroma after `stride * height` bytes.
struct Nv12 {
    const uint8_t* data = nullptr;
    size_t size = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t stride = 0;
};

// A region of the host's picture.
struct Crop {
    uint32_t x = 0;
    uint32_t y = 0;
    uint32_t width = 0;
    uint32_t height = 0;
};

// The region of a `pictureWidth` by `pictureHeight` picture a `width` by `height` buffer is
// filled from: the largest one of the buffer's shape, at the centre, as Android's centre-only
// cropping has it, so a 4:3 stream of a 16:9 picture is not stretched.
Crop centerCrop(uint32_t pictureWidth, uint32_t pictureHeight, uint32_t width, uint32_t height);

// Whether `picture` holds all the bytes its size says.
bool isWhole(const Nv12& picture);

// Scales `picture` into the `width` by `height` buffer whose planes are `planes`. Returns
// false, writing nothing, when the picture is not whole or a size is 0.
bool scaleToYuv(const Nv12& picture, uint32_t width, uint32_t height, const YuvPlanes& planes);

// Scales `picture` into the `width` by `height` RGBA buffer whose rows are `rowBytes` apart,
// with the limited range BT.601 colours a camera's YUV carries.
bool scaleToRgba(const Nv12& picture, uint32_t width, uint32_t height, uint8_t* rgba, uint32_t rowBytes);

// Writes the JPEG blob's trailer, which says how long the JPEG at the buffer's start is, at the
// end of a BLOB buffer of `bufferSize` bytes, after copying `jpeg` to its start. False when
// the JPEG and the trailer do not fit.
bool writeJpegBlob(uint8_t* buffer, size_t bufferSize, const uint8_t* jpeg, size_t jpegSize);

}  // namespace dsa::camera
