// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Copying what the GPU drew back into a gralloc buffer's memory, for a CPU that reads it.
//
// A Hyper-V VM has no virtio-gpu device, so a gralloc buffer the GPU draws into has two
// copies: the guest memory minigbm allocated, which a CPU lock maps, and the host ColorBuffer
// the drawing really goes to. Nothing moves the drawing from the second to the first, so
// `screencap`, an IDE's screenshot and an ImageReader that reads frames on the CPU all saw
// zeros. On a virtio-gpu device the kernel does this move when the buffer is mapped.
//
// The guest Vulkan driver now does it, at the point the drawing is finished for the CPU: when
// it hands out a fence or semaphore for the work (SurfaceFlinger's capture, which screencap
// waits on), when it waits for that work itself, and when it releases a window image to its
// consumer (an ImageReader). It reads the ColorBuffer back from the host and writes it over
// the buffer's memory, row by row at the buffer's stride.
//
// Only buffers a CPU reads and the GPU draws into, and the CPU does not write, are read back.
// Every other buffer pays nothing: a process with no such buffer never waits or reads, and
// one with them reads only those a submit since the last read back may have drawn into. A
// buffer the CPU also writes keeps what the CPU wrote, which a read back would overwrite.
// The one exception is a buffer the composer also uses: SurfaceFlinger's capture asks for CPU
// writes too (GRALLOC_USAGE_SW_WRITE_OFTEN, beside the read), but only the GPU draws into it,
// and without it screencap read zeros.

#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <unordered_map>
#include <vector>

namespace dsa {

// Android's gralloc usage bits (AHARDWAREBUFFER_USAGE_*), mirrored to keep this header free of
// platform headers.
inline constexpr uint64_t kUsageCpuReadMask = 0xFULL;
inline constexpr uint64_t kUsageCpuWriteMask = 0xF0ULL;
inline constexpr uint64_t kUsageGpuColorOutput = 1ULL << 9;
inline constexpr uint64_t kUsageComposerOverlay = 1ULL << 11;

// Whether a buffer with this usage gets the GPU's drawing copied back for the CPU.
inline bool wantsCpuReadback(uint64_t usage) {
    return (usage & kUsageCpuReadMask) != 0 && (usage & kUsageGpuColorOutput) != 0 &&
           ((usage & kUsageCpuWriteMask) == 0 || (usage & kUsageComposerOverlay) != 0);
}

// How one buffer format comes back from the host.
struct ReadbackFormat {
    // What the guest asks rcReadColorBuffer for. Its pixel size must be the size of the host
    // ColorBuffer's own pixels: the host sends those bytes as they are.
    uint32_t glFormat = 0;
    uint32_t glType = 0;
    // A pixel's size in what the host sends, and in the guest buffer.
    uint32_t hostBytesPerPixel = 0;
    uint32_t guestBytesPerPixel = 0;
};

namespace readback_gl {
inline constexpr uint32_t kRgb = 0x1907;
inline constexpr uint32_t kRgba = 0x1908;
inline constexpr uint32_t kLuminance = 0x1909;
inline constexpr uint32_t kBgraExt = 0x80E1;
inline constexpr uint32_t kUnsignedByte = 0x1401;
inline constexpr uint32_t kHalfFloat = 0x140B;
inline constexpr uint32_t kUnsignedShort565 = 0x8363;
inline constexpr uint32_t kUnsignedInt2101010Rev = 0x8368;
}  // namespace readback_gl

constexpr uint32_t readbackFourcc(char a, char b, char c, char d) {
    return uint32_t(uint8_t(a)) | (uint32_t(uint8_t(b)) << 8) | (uint32_t(uint8_t(c)) << 16) |
           (uint32_t(uint8_t(d)) << 24);
}

// The format to read a buffer of this DRM format back in, from the ColorBuffer format
// dsa_shared_color_buffer_gl_format() gives it. False for a format with no ColorBuffer of its
// own (YUV), whose ColorBuffer does not hold the buffer's layout.
inline bool readbackFormat(uint32_t drmFourcc, ReadbackFormat* out) {
    using namespace readback_gl;
    switch (drmFourcc) {
        case readbackFourcc('A', 'B', '2', '4'):  // RGBA_8888: an RGBA ColorBuffer
        case readbackFourcc('X', 'B', '2', '4'):  // RGBX_8888
            *out = {kRgba, kUnsignedByte, 4, 4};
            return true;
        case readbackFourcc('A', 'R', '2', '4'):  // BGRA_8888: a BGRA ColorBuffer
        case readbackFourcc('X', 'R', '2', '4'):
            *out = {kBgraExt, kUnsignedByte, 4, 4};
            return true;
        case readbackFourcc('B', 'G', '2', '4'):  // RGB_888: the host keeps it as RGBA
            *out = {kRgba, kUnsignedByte, 4, 3};
            return true;
        case readbackFourcc('R', 'G', '1', '6'):  // RGB_565
            *out = {kRgb, kUnsignedShort565, 2, 2};
            return true;
        case readbackFourcc('A', 'B', '4', 'H'):  // RGBA_FP16
            *out = {kRgba, kHalfFloat, 8, 8};
            return true;
        case readbackFourcc('A', 'B', '3', '0'):  // RGBA_1010102
            *out = {kRgba, kUnsignedInt2101010Rev, 4, 4};
            return true;
        case readbackFourcc('R', '8', ' ', ' '):  // R_8
            *out = {kLuminance, kUnsignedByte, 1, 1};
            return true;
        default:
            return false;
    }
}

// The bytes the host sends for a whole buffer: its rows packed, with no gap between them.
inline size_t hostReadbackSize(const ReadbackFormat& format, uint32_t width, uint32_t height) {
    return size_t(format.hostBytesPerPixel) * width * height;
}

// Writes the host's packed rows into the guest buffer, whose rows are `guestStride` bytes
// apart in the `guestSize` bytes from `guest`. False, with nothing written, when the buffer
// is too small for its size and stride or the stride for a row.
inline bool copyReadbackRows(const uint8_t* host, size_t hostSize, const ReadbackFormat& format,
                             uint32_t width, uint32_t height, uint8_t* guest, size_t guestStride,
                             size_t guestSize) {
    if (width == 0 || height == 0) return true;
    const size_t hostRow = size_t(format.hostBytesPerPixel) * width;
    const size_t guestRow = size_t(format.guestBytesPerPixel) * width;
    if (hostSize < hostRow * height || guestStride < guestRow ||
        guestSize < guestStride * (height - 1) + guestRow) {
        return false;
    }
    for (uint32_t y = 0; y < height; y++) {
        const uint8_t* from = host + hostRow * y;
        uint8_t* to = guest + guestStride * y;
        if (format.hostBytesPerPixel == format.guestBytesPerPixel) {
            std::memcpy(to, from, guestRow);
            continue;
        }
        // Fewer bytes in the guest (RGB_888 from RGBA): its pixel's first bytes, in order.
        for (uint32_t x = 0; x < width; x++) {
            std::memcpy(to + size_t(format.guestBytesPerPixel) * x,
                        from + size_t(format.hostBytesPerPixel) * x, format.guestBytesPerPixel);
        }
    }
    return true;
}

// The buffers of one process that are read back, by the Vulkan memory they are imported as.
// Not thread safe: the driver holds its lock around every call.
class CpuReadbackSet {
   public:
    void add(uint64_t memory, uint64_t device, void* buffer) {
        mEntries[memory] = Entry{device, buffer, false};
    }
    void remove(uint64_t memory) { mEntries.erase(memory); }
    bool empty() const { return mEntries.empty(); }

    // A submit on `device`: the GPU may draw into any of its buffers. With no buffer to read
    // back, which is every process but the few that read the GPU's drawing, this does nothing.
    void submitted(uint64_t device) {
        for (auto& item : mEntries) {
            if (item.second.device == device) item.second.pending = true;
        }
    }

    // The buffers of `device` a submit may have drawn into since they were last read back.
    // They are no longer pending: the caller reads them back now.
    std::vector<void*> takePending(uint64_t device) {
        std::vector<void*> buffers;
        for (auto& item : mEntries) {
            Entry& entry = item.second;
            if (entry.device == device && entry.pending) {
                entry.pending = false;
                buffers.push_back(entry.buffer);
            }
        }
        return buffers;
    }

   private:
    struct Entry {
        uint64_t device;
        void* buffer;
        bool pending;
    };
    std::unordered_map<uint64_t, Entry> mEntries;
};

}  // namespace dsa
