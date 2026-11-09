// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Copying what the GPU drew back into a gralloc buffer for the CPU (dsa_cpu_readback.h), run
// on the build machine.

#include "dsa_cpu_readback.h"

#include <cstdint>
#include <cstdio>
#include <vector>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

// AHARDWAREBUFFER_USAGE_* values, as Android defines them.
constexpr uint64_t kCpuReadRarely = 2;
constexpr uint64_t kCpuReadOften = 3;
constexpr uint64_t kCpuWriteOften = 3 << 4;
constexpr uint64_t kGpuSampled = 1 << 8;
constexpr uint64_t kGpuColorOutput = 1 << 9;
constexpr uint64_t kComposerOverlay = 1 << 11;

static void pr3_only_a_buffer_the_gpu_draws_and_the_cpu_only_reads_is_read_back() {
    // SurfaceFlinger's capture for screencap, and an ImageReader a renderer draws into.
    EXPECT(dsa::wantsCpuReadback(kGpuColorOutput | kGpuSampled | kComposerOverlay | kCpuReadOften));
    // SurfaceFlinger's capture as Android 16 and 17 really allocate it, CPU writes included
    // (image test run 42539160677 read zeros from every screencap without it).
    EXPECT(dsa::wantsCpuReadback(kGpuColorOutput | kGpuSampled | kComposerOverlay | kCpuReadOften |
                                 kCpuWriteOften));
    EXPECT(dsa::wantsCpuReadback(kGpuColorOutput | kCpuReadRarely));
    // A window an app draws for the screen: no CPU reads it, so it pays nothing.
    EXPECT(!dsa::wantsCpuReadback(kGpuColorOutput | kGpuSampled | kComposerOverlay));
    // A buffer the CPU fills (a software canvas, a camera frame): what it wrote stays.
    EXPECT(!dsa::wantsCpuReadback(kCpuReadOften | kCpuWriteOften | kGpuSampled));
    EXPECT(!dsa::wantsCpuReadback(kGpuColorOutput | kCpuReadOften | kCpuWriteOften));
    // One the GPU only samples has no drawing of its own to bring back.
    EXPECT(!dsa::wantsCpuReadback(kGpuSampled | kCpuReadOften));
}

static void pr3_each_format_asks_the_host_for_its_colorbuffers_own_pixel_size() {
    dsa::ReadbackFormat format;
    EXPECT(dsa::readbackFormat(dsa::readbackFourcc('A', 'B', '2', '4'), &format));
    EXPECT(format.glFormat == dsa::readback_gl::kRgba && format.glType == dsa::readback_gl::kUnsignedByte);
    EXPECT(format.hostBytesPerPixel == 4 && format.guestBytesPerPixel == 4);

    EXPECT(dsa::readbackFormat(dsa::readbackFourcc('A', 'R', '2', '4'), &format));
    EXPECT(format.glFormat == dsa::readback_gl::kBgraExt && format.hostBytesPerPixel == 4);

    // The host keeps RGB_888 as RGBA, and the buffer has three bytes a pixel.
    EXPECT(dsa::readbackFormat(dsa::readbackFourcc('B', 'G', '2', '4'), &format));
    EXPECT(format.glFormat == dsa::readback_gl::kRgba);
    EXPECT(format.hostBytesPerPixel == 4 && format.guestBytesPerPixel == 3);

    EXPECT(dsa::readbackFormat(dsa::readbackFourcc('R', 'G', '1', '6'), &format));
    EXPECT(format.glType == dsa::readback_gl::kUnsignedShort565 && format.hostBytesPerPixel == 2);

    EXPECT(dsa::readbackFormat(dsa::readbackFourcc('A', 'B', '4', 'H'), &format));
    EXPECT(format.glType == dsa::readback_gl::kHalfFloat && format.hostBytesPerPixel == 8);

    EXPECT(dsa::readbackFormat(dsa::readbackFourcc('A', 'B', '3', '0'), &format));
    EXPECT(format.glType == dsa::readback_gl::kUnsignedInt2101010Rev && format.hostBytesPerPixel == 4);

    EXPECT(dsa::readbackFormat(dsa::readbackFourcc('R', '8', ' ', ' '), &format));
    EXPECT(format.hostBytesPerPixel == 1 && format.guestBytesPerPixel == 1);

    // YUV has no ColorBuffer of its own layout: nothing is read back over it.
    EXPECT(!dsa::readbackFormat(dsa::readbackFourcc('N', 'V', '1', '2'), &format));
    EXPECT(!dsa::readbackFormat(dsa::readbackFourcc('Y', 'V', '1', '2'), &format));

    EXPECT(dsa::hostReadbackSize({dsa::readback_gl::kRgba, dsa::readback_gl::kUnsignedByte, 4, 4},
                                 1920, 1080) == 1920u * 1080u * 4u);
}

static void pr3_the_drawing_lands_on_each_row_at_the_buffers_stride() {
    const dsa::ReadbackFormat rgba{dsa::readback_gl::kRgba, dsa::readback_gl::kUnsignedByte, 4, 4};
    // 2x2, packed as the host sends it.
    const std::vector<uint8_t> host = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16};
    // Rows 12 bytes apart: 8 of pixels and 4 of padding, which keeps what it had.
    std::vector<uint8_t> guest(24, 0xEE);
    EXPECT(dsa::copyReadbackRows(host.data(), host.size(), rgba, 2, 2, guest.data(), 12, guest.size()));
    const std::vector<uint8_t> expected = {1, 2, 3, 4, 5, 6, 7, 8, 0xEE, 0xEE, 0xEE, 0xEE,
                                           9, 10, 11, 12, 13, 14, 15, 16, 0xEE, 0xEE, 0xEE, 0xEE};
    EXPECT(guest == expected);

    // The last row needs no padding after it.
    std::vector<uint8_t> tight(20, 0);
    EXPECT(dsa::copyReadbackRows(host.data(), host.size(), rgba, 2, 2, tight.data(), 12, tight.size()));
    EXPECT(tight[12] == 9 && tight[19] == 16);
}

static void pr3_rgb_888_keeps_each_pixels_first_three_bytes() {
    const dsa::ReadbackFormat rgb{dsa::readback_gl::kRgba, dsa::readback_gl::kUnsignedByte, 4, 3};
    const std::vector<uint8_t> host = {10, 20, 30, 255, 40, 50, 60, 255};
    std::vector<uint8_t> guest(6, 0);
    EXPECT(dsa::copyReadbackRows(host.data(), host.size(), rgb, 2, 1, guest.data(), 6, guest.size()));
    EXPECT((guest == std::vector<uint8_t>{10, 20, 30, 40, 50, 60}));
}

static void nfr5_a_buffer_too_small_for_its_drawing_is_left_alone() {
    const dsa::ReadbackFormat rgba{dsa::readback_gl::kRgba, dsa::readback_gl::kUnsignedByte, 4, 4};
    const std::vector<uint8_t> host(16, 7);
    std::vector<uint8_t> guest(15, 0);
    // Too small for two rows of 8 bytes.
    EXPECT(!dsa::copyReadbackRows(host.data(), host.size(), rgba, 2, 2, guest.data(), 8, guest.size()));
    // A stride shorter than a row.
    EXPECT(!dsa::copyReadbackRows(host.data(), host.size(), rgba, 2, 2, guest.data(), 4, guest.size()));
    // Less from the host than the buffer's size.
    std::vector<uint8_t> roomy(64, 0);
    EXPECT(!dsa::copyReadbackRows(host.data(), 8, rgba, 2, 2, roomy.data(), 8, roomy.size()));
    bool untouched = true;
    for (uint8_t byte : guest) untouched = untouched && byte == 0;
    for (uint8_t byte : roomy) untouched = untouched && byte == 0;
    EXPECT(untouched);
    // An empty buffer has nothing to copy.
    EXPECT(dsa::copyReadbackRows(nullptr, 0, rgba, 0, 0, nullptr, 0, 0));
}

static void pr3_only_buffers_a_submit_may_have_drawn_are_read_back_once() {
    dsa::CpuReadbackSet set;
    EXPECT(set.empty());
    // A process with nothing to read back: a submit marks nothing, a wait reads nothing.
    set.submitted(1);
    EXPECT(set.takePending(1).empty());

    int capture = 0;
    int reader = 0;
    int other = 0;
    set.add(100, 1, &capture);
    set.add(101, 1, &reader);
    set.add(200, 2, &other);
    EXPECT(!set.empty());
    // Imported, but nothing drew into it yet.
    EXPECT(set.takePending(1).empty());

    set.submitted(1);
    auto pending = set.takePending(1);
    EXPECT(pending.size() == 2);
    // Another device's buffer waits for a submit on its own device.
    EXPECT(set.takePending(2).empty());
    // Read back once per submit, not at every wait after it.
    EXPECT(set.takePending(1).empty());

    set.remove(100);
    set.submitted(1);
    pending = set.takePending(1);
    EXPECT(pending.size() == 1 && pending[0] == &reader);

    set.remove(101);
    set.remove(200);
    EXPECT(set.empty());
}

int main() {
    pr3_only_a_buffer_the_gpu_draws_and_the_cpu_only_reads_is_read_back();
    pr3_each_format_asks_the_host_for_its_colorbuffers_own_pixel_size();
    pr3_the_drawing_lands_on_each_row_at_the_buffers_stride();
    pr3_rgb_888_keeps_each_pixels_first_three_bytes();
    nfr5_a_buffer_too_small_for_its_drawing_is_left_alone();
    pr3_only_buffers_a_submit_may_have_drawn_are_read_back_once();
    if (failures) {
        std::fprintf(stderr, "%d expectation(s) failed\n", failures);
        return 1;
    }
    std::printf("cpu readback: all tests passed\n");
    return 0;
}
