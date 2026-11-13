// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Copies a picture the host decoded (NV12) into the planes of a guest graphics buffer, laid
// out however the buffer's allocator lays out YUV 4:2:0: separate or interleaved chroma, and
// any row and pixel stride.

#pragma once

#include <cstddef>
#include <cstdint>
#include <functional>

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

// As above, into a buffer of `bufferWidth` by `bufferHeight` (at least the picture's size),
// whose samples past the picture take the picture's edge, so nothing outside the picture is
// ever zero (which shows as green). False, copying nothing, when the buffer is smaller.
bool copyNv12(const uint8_t* nv12, size_t size, uint32_t width, uint32_t height, uint32_t stride,
              const YuvPlanes& planes, uint32_t bufferWidth, uint32_t bufferHeight);

// How writing a picture into an output buffer went. Only a buffer that is `Written` holds the
// picture; any other goes back to its allocator unseen, because a buffer of the surface that
// was not written holds zeros (green) or an older picture.
enum class WriteResult {
    Written,
    // The buffer is smaller than the picture.
    WrongBuffer,
    // The buffer could not be locked for writing.
    NotLocked,
    // The lock gave fewer than three planes, or rows too short for the buffer's width.
    BadPlanes,
    // The picture is shorter than its size says.
    NotCopied,
    // The unlock failed, so the writes may not have reached the buffer.
    NotUnlocked,
};

// The size of a buffer the allocator gave.
struct BufferSize {
    uint32_t width = 0;
    uint32_t height = 0;
};

// Locks a buffer for writing: true with its planes and how many there are.
using LockPlanes = std::function<bool(YuvPlanes* planes, uint32_t* planeCount)>;
// Unlocks it again: true when the writes reached the buffer.
using UnlockPlanes = std::function<bool()>;

// Writes the `width` by `height` NV12 picture in `nv12` into a buffer of `buffer`'s size,
// every sample of it (past the picture, the picture's edge). A buffer that was locked is
// always unlocked.
WriteResult writePicture(const uint8_t* nv12, size_t size, uint32_t width, uint32_t height, uint32_t stride,
                         const BufferSize& buffer, const LockPlanes& lock, const UnlockPlanes& unlock);

}  // namespace dsa::codec2
