// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The rules for a decoder's output buffers: the format and size they are allocated in, the
// rectangle of each that is the picture, and what an allocation that failed means.
//
// An app that shows its video hands the decoder its surface's buffers, which the framework's
// IGraphicBufferAllocator dequeues for the component. Those buffers must suit the surface's
// consumer (the composer), and the guest's gralloc (minigbm, with no GPU of its own) has no
// NV12 buffer the composer may use: every dequeue of one failed, the allocator answered
// C2_CORRUPTED, and no picture reached the screen. YV12 is what the platform's own software
// decoders allocate for a surface, and the gralloc gives it to the composer, so the
// component allocates YV12 too.
//
// While every buffer of the surface is shown or queued, the allocator answers C2_BLOCKING
// (a positive c2_status_t, as the framework's allocator sends it) and its waitable file
// descriptor becomes readable once one comes back: the component waits on it rather than
// dropping the picture.

#pragma once

#include <cstdint>

namespace dsa::codec2 {

// HAL_PIXEL_FORMAT_YV12 (AHARDWAREBUFFER_FORMAT_YV12): 8-bit 4:2:0, the Cr plane then the Cb
// plane, each row 16-byte aligned.
inline constexpr uint32_t kOutputFormat = 0x32315659;

// c2_status_t values, as the framework's IGraphicBufferAllocator answers with them.
inline constexpr int32_t kC2BadState = 1;
inline constexpr int32_t kC2Canceled = 4;
inline constexpr int32_t kC2Blocking = 11;
inline constexpr int32_t kC2TimedOut = 110;

// What a failed allocation from the surface's allocator means.
enum class AllocateFailure {
    // Every buffer is in use: wait for the waitable descriptor, then ask again.
    Wait,
    // The allocator stopped (the codec is stopping or its surface went away): give up quietly.
    Stopped,
    // Anything else: this picture cannot have a buffer.
    Failed,
};

AllocateFailure classifyAllocateError(int32_t serviceSpecificError);

// Binder's EX_TRANSACTION_FAILED and DEAD_OBJECT, as a failed call's status carries them.
inline constexpr int32_t kExTransactionFailed = -129;
inline constexpr int32_t kStatusDeadObject = -32;

// Whether a binder call to the framework failed because its process is gone (the app died):
// its exception code and its status (AStatus_getExceptionCode, AStatus_getStatus). Nothing
// that process held will be called back again, so the component stops at once rather than
// try every picture of the stream against it.
bool peerIsGone(int32_t exceptionCode, int32_t status);

struct PictureSize {
    uint32_t width = 0;
    uint32_t height = 0;
};

// The size of the buffer a `width` by `height` picture is allocated in: the size rounded up to
// 2, which 4:2:0 needs, and no more, so no padding is ever left to show.
PictureSize bufferSizeFor(uint32_t width, uint32_t height);

struct Crop {
    uint32_t left = 0;
    uint32_t top = 0;
    uint32_t width = 0;
    uint32_t height = 0;
};

// The rectangle of a buffer that is the picture: the visible size, clamped to the buffer.
Crop cropFor(uint32_t width, uint32_t height, const PictureSize& buffer);

// Says whether a failure should be logged: the first of a run, then one in `every`, so a
// failure on every picture does not flood the log. `reset()` once an allocation succeeds.
class FailureThrottle {
   public:
    explicit FailureThrottle(uint32_t every = 100) : mEvery(every) {}
    // True when this failure should be logged; `*count` is how many there have been in a row.
    bool shouldLog(uint32_t* count);
    void reset() { mCount = 0; }

   private:
    uint32_t mEvery;
    uint32_t mCount = 0;
};

}  // namespace dsa::codec2
