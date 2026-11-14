// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Copying what a CPU wrote into a gralloc buffer up to its host ColorBuffer
// (dsa_cpu_upload.h), run on the build machine.

#include "dsa_cpu_upload.h"
#include "dsa_shared_color_buffer.h"

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
constexpr uint64_t kCpuReadOften = 3;
constexpr uint64_t kCpuWriteRarely = 2 << 4;
constexpr uint64_t kCpuWriteOften = 3 << 4;
constexpr uint64_t kGpuSampled = 1 << 8;
constexpr uint64_t kGpuColorOutput = 1 << 9;
constexpr uint64_t kComposerOverlay = 1 << 11;
constexpr uint64_t kVideoDecoder = 1ULL << 22;

constexpr uint32_t kYv12 = dsa::uploadFourcc('9', '9', '9', '7');
constexpr uint32_t kNv12 = dsa::uploadFourcc('N', 'V', '1', '2');

static void pr4_only_a_buffer_the_cpu_writes_and_the_gpu_does_not_draw_is_copied_up() {
    // A video decoder's surface buffers (dsa-codec2 and the platform's software decoders).
    EXPECT(dsa::wantsCpuUpload(kCpuWriteOften | kGpuSampled | kComposerOverlay | kVideoDecoder));
    // A camera frame, and a layer an app draws in software.
    EXPECT(dsa::wantsCpuUpload(kCpuWriteOften | kCpuReadOften | kGpuSampled));
    EXPECT(dsa::wantsCpuUpload(kCpuWriteRarely | kGpuSampled | kComposerOverlay));
    // A window the GPU draws, and SurfaceFlinger's capture (CPU writes asked for, but only the
    // GPU draws): the host holds their drawing already, so they cost nothing.
    EXPECT(!dsa::wantsCpuUpload(kGpuColorOutput | kGpuSampled | kComposerOverlay));
    EXPECT(!dsa::wantsCpuUpload(kGpuColorOutput | kGpuSampled | kComposerOverlay | kCpuReadOften |
                                kCpuWriteOften));
    EXPECT(!dsa::wantsCpuUpload(kGpuSampled | kCpuReadOften));
}

static void pr4_yuv_planes_go_up_as_the_hosts_image_lays_them_out() {
    // The decoders' 426x240 pictures: Y, then two chroma planes at half size, in one row of
    // bytes whose count the host checks against its image.
    const dsa::UploadLayout three = dsa::uploadLayout(kYv12, 426, 240, true);
    EXPECT(three.kind == dsa::UploadKind::Yuv420ThreePlane);
    EXPECT(three.planeCount == 3);
    EXPECT(three.planes[0].rowBytes == 426 && three.planes[0].rows == 240);
    EXPECT(three.planes[1].rowBytes == 213 && three.planes[1].rows == 120);
    EXPECT(three.planes[2].rowBytes == 213 && three.planes[2].rows == 120);
    EXPECT(three.packedSize() == 426u * 240 + 2u * 213 * 120);
    EXPECT(three.callWidth == static_cast<int32_t>(three.packedSize()) && three.callHeight == 1);
    EXPECT(three.glFormat == 0x1909 && three.glType == 0x1401);

    const dsa::UploadLayout two = dsa::uploadLayout(kNv12, 360, 640, true);
    EXPECT(two.kind == dsa::UploadKind::Yuv420TwoPlane);
    EXPECT(two.planeCount == 2);
    EXPECT(two.planes[1].rowBytes == 360 && two.planes[1].rows == 320);
    EXPECT(two.packedSize() == 360u * 640 + 360u * 320);

    // Without YUV ColorBuffers the buffer's is RGBA, which its planes do not fit; an odd size
    // has an RGBA ColorBuffer too (dsa_shared_color_buffer_create_format).
    EXPECT(!dsa::uploadLayout(kYv12, 426, 240, false).valid());
    EXPECT(!dsa::uploadLayout(kYv12, 427, 240, true).valid());
    EXPECT(!dsa::uploadLayout(kNv12, 360, 641, true).valid());
    EXPECT(dsa_shared_color_buffer_create_format(kYv12, 426, 240, 1) ==
           DSA_COLOR_BUFFER_YUV420_THREE_PLANE);
    EXPECT(dsa_shared_color_buffer_create_format(kYv12, 427, 240, 1) == 0x1908u);
}

static void pr4_rgb_buffers_go_up_at_their_colorbuffers_pixel_size() {
    const dsa::UploadLayout rgba = dsa::uploadLayout(dsa::uploadFourcc('A', 'B', '2', '4'), 5, 3, false);
    EXPECT(rgba.kind == dsa::UploadKind::Rgb && rgba.planeCount == 1);
    EXPECT(rgba.planes[0].rowBytes == 20 && rgba.planes[0].rows == 3);
    EXPECT(rgba.callWidth == 5 && rgba.callHeight == 3);
    EXPECT(rgba.glFormat == 0x1908 && rgba.glType == 0x1401);
    const dsa::UploadLayout rgb565 = dsa::uploadLayout(dsa::uploadFourcc('R', 'G', '1', '6'), 5, 3, true);
    EXPECT(rgb565.planes[0].rowBytes == 10 && rgb565.glType == 0x8363);
    const dsa::UploadLayout rgb888 = dsa::uploadLayout(dsa::uploadFourcc('B', 'G', '2', '4'), 5, 3, true);
    EXPECT(rgb888.planes[0].rowBytes == 15 && rgb888.glFormat == 0x1907);
    // Formats whose ColorBuffer takes other pixels are not copied.
    EXPECT(!dsa::uploadLayout(dsa::uploadFourcc('A', 'B', '4', 'H'), 5, 3, true).valid());
    EXPECT(!dsa::uploadLayout(dsa::uploadFourcc('A', 'B', '3', '0'), 5, 3, true).valid());
    EXPECT(!dsa::uploadLayout(dsa::uploadFourcc('A', 'B', '2', '4'), 0, 3, true).valid());
}

// A YV12 buffer as minigbm lays it out: Y rows 16-byte aligned, then Cr, then Cb, each chroma
// row aligned to 16 too. Every sample says which plane and row it is.
struct Yv12 {
    uint32_t width = 0, height = 0;
    std::vector<uint8_t> bytes;
    dsa::SourcePlane planes[3];
};

static Yv12 makeYv12(uint32_t width, uint32_t height) {
    Yv12 b;
    b.width = width;
    b.height = height;
    const uint32_t yStride = (width + 15) / 16 * 16;
    const uint32_t cStride = (yStride / 2 + 15) / 16 * 16;
    b.planes[0] = {0, yStride};
    b.planes[1] = {uint64_t(yStride) * height, cStride};
    b.planes[2] = {b.planes[1].offset + uint64_t(cStride) * (height / 2), cStride};
    b.bytes.assign(b.planes[2].offset + uint64_t(cStride) * (height / 2), 0xEE);
    for (uint32_t p = 0; p < 3; ++p) {
        const uint32_t w = p == 0 ? width : width / 2;
        const uint32_t h = p == 0 ? height : height / 2;
        for (uint32_t y = 0; y < h; ++y) {
            for (uint32_t x = 0; x < w; ++x) {
                b.bytes[b.planes[p].offset + uint64_t(b.planes[p].stride) * y + x] =
                    static_cast<uint8_t>(p * 80 + (y + x) % 64);
            }
        }
    }
    return b;
}

static void pr4_planes_are_packed_without_row_padding_in_the_buffers_plane_order() {
    const Yv12 b = makeYv12(426, 240);
    const dsa::UploadLayout layout = dsa::uploadLayout(kYv12, 426, 240, true);
    std::vector<uint8_t> packed(layout.packedSize(), 0);
    EXPECT(dsa::packPlanes(b.bytes.data(), b.bytes.size(), b.planes, 3, layout, packed.data()));
    bool exact = true;
    size_t at = 0;
    for (uint32_t p = 0; p < 3; ++p) {
        for (uint32_t y = 0; y < layout.planes[p].rows; ++y) {
            for (uint32_t x = 0; x < layout.planes[p].rowBytes; ++x) {
                // No padding (0xEE) comes along, and the second plane stays the buffer's
                // second (Cr for YV12: the guest driver swizzles it).
                exact = exact && packed[at++] == static_cast<uint8_t>(p * 80 + (y + x) % 64);
            }
        }
    }
    EXPECT(exact);
    EXPECT(at == packed.size());
}

// A 360x640 Shorts picture as the codec HAL's surface buffers hold it: minigbm gives YV12 luma
// rows of ALIGN(360, 32) = 384 bytes and chroma rows of 192, so the rows the host takes are
// shorter than the buffer's. Every row from the first goes up from its own place.
static void fr26_a_portrait_short_goes_up_from_its_top_row() {
    const uint32_t width = 360, height = 640, yStride = 384, cStride = 192;
    Yv12 b;
    b.width = width;
    b.height = height;
    b.planes[0] = {0, yStride};
    b.planes[1] = {uint64_t(yStride) * height, cStride};
    b.planes[2] = {b.planes[1].offset + uint64_t(cStride) * (height / 2), cStride};
    b.bytes.assign(b.planes[2].offset + uint64_t(cStride) * (height / 2), 0);
    for (uint32_t p = 0; p < 3; ++p) {
        const uint32_t w = p == 0 ? width : width / 2;
        const uint32_t h = p == 0 ? height : height / 2;
        for (uint32_t y = 0; y < h; ++y) {
            for (uint32_t x = 0; x < w; ++x) {
                b.bytes[b.planes[p].offset + uint64_t(b.planes[p].stride) * y + x] =
                    static_cast<uint8_t>(1 + p * 80 + y % 79);
            }
        }
    }
    const dsa::UploadLayout layout = dsa::uploadLayout(kYv12, width, height, true);
    EXPECT(layout.planes[0].rowBytes == 360 && layout.planes[0].rows == 640);
    EXPECT(layout.planes[1].rowBytes == 180 && layout.planes[1].rows == 320);
    std::vector<uint8_t> packed(layout.packedSize(), 0);
    EXPECT(dsa::packPlanes(b.bytes.data(), b.bytes.size(), b.planes, 3, layout, packed.data()));
    // Each packed row is its own row of its plane: none is shifted, none is padding (0).
    bool exact = true;
    size_t at = 0;
    for (uint32_t p = 0; p < 3; ++p) {
        for (uint32_t y = 0; y < layout.planes[p].rows; ++y) {
            for (uint32_t x = 0; x < layout.planes[p].rowBytes; ++x) {
                exact = exact && packed[at++] == static_cast<uint8_t>(1 + p * 80 + y % 79);
            }
        }
    }
    EXPECT(exact);
    EXPECT(at == packed.size());
}

static void nfr5_planes_outside_the_mapping_are_not_read() {
    const Yv12 b = makeYv12(64, 32);
    const dsa::UploadLayout layout = dsa::uploadLayout(kYv12, 64, 32, true);
    std::vector<uint8_t> packed(layout.packedSize());
    // A mapping one byte short of the last plane.
    EXPECT(!dsa::packPlanes(b.bytes.data(), b.bytes.size() - 1, b.planes, 3, layout, packed.data()));
    EXPECT(dsa::fingerprint(b.bytes.data(), b.bytes.size() - 1, b.planes, 3, layout) == 0);
    // A stride shorter than a row, and too few planes.
    dsa::SourcePlane narrow[3] = {b.planes[0], b.planes[1], b.planes[2]};
    narrow[0].stride = 10;
    EXPECT(!dsa::packPlanes(b.bytes.data(), b.bytes.size(), narrow, 3, layout, packed.data()));
    EXPECT(!dsa::packPlanes(b.bytes.data(), b.bytes.size(), b.planes, 2, layout, packed.data()));
    EXPECT(!dsa::packPlanes(b.bytes.data(), b.bytes.size(), b.planes, 3, dsa::UploadLayout{},
                            packed.data()));
}

static void pr4_a_picture_is_copied_once_and_again_only_when_it_changes() {
    Yv12 b = makeYv12(64, 32);
    const dsa::UploadLayout layout = dsa::uploadLayout(kYv12, 64, 32, true);
    const uint64_t first = dsa::fingerprint(b.bytes.data(), b.bytes.size(), b.planes, 3, layout);
    EXPECT(first != 0);
    EXPECT(dsa::fingerprint(b.bytes.data(), b.bytes.size(), b.planes, 3, layout) == first);

    dsa::UploadTracker tracker;
    EXPECT(tracker.needsUpload(7, first, false));  // never copied
    tracker.uploaded(7, first);
    EXPECT(!tracker.needsUpload(7, first, false));  // the same picture again: no copy
    EXPECT(tracker.needsUpload(7, first, true));    // a layer's new buffer is always copied

    // A row the fingerprint reads changes: copied again. A row it skips changes nothing (a
    // layer's new buffer is copied whole whatever its fingerprint).
    b.bytes[b.planes[0].offset + uint64_t(b.planes[0].stride) * 8 + 3] ^= 0x55;
    const uint64_t changed = dsa::fingerprint(b.bytes.data(), b.bytes.size(), b.planes, 3, layout);
    EXPECT(changed != first);
    EXPECT(tracker.needsUpload(7, changed, false));
    b.bytes[b.planes[0].offset + uint64_t(b.planes[0].stride) * 1 + 5] ^= 0x55;  // Y row 1
    EXPECT(dsa::fingerprint(b.bytes.data(), b.bytes.size(), b.planes, 3, layout) == changed);
    // The chroma planes count too.
    b.bytes[b.planes[2].offset + uint64_t(b.planes[2].stride) * 15] ^= 0x01;  // Cb's last row
    EXPECT(dsa::fingerprint(b.bytes.data(), b.bytes.size(), b.planes, 3, layout) != changed);

    tracker.forget(7);
    EXPECT(tracker.size() == 0);
    EXPECT(tracker.needsUpload(7, changed, false));
}

static void pr4_the_bytes_copied_are_reported_once_a_second() {
    dsa::UploadRate rate;
    dsa::UploadRate::Report report;
    EXPECT(!rate.add(1000, 100, &report));
    EXPECT(!rate.add(500000000, 200, &report));
    EXPECT(rate.add(1000001000, 300, &report));
    EXPECT(report.bytes == 600 && report.uploads == 3 && report.windowNs == 1000000000);
    // The next window starts with the next copy.
    EXPECT(!rate.add(5000000000ULL, 50, &report));
    EXPECT(rate.add(6000000000ULL, 50, &report));
    EXPECT(report.bytes == 100 && report.uploads == 2);
}

int main() {
    pr4_only_a_buffer_the_cpu_writes_and_the_gpu_does_not_draw_is_copied_up();
    pr4_yuv_planes_go_up_as_the_hosts_image_lays_them_out();
    pr4_rgb_buffers_go_up_at_their_colorbuffers_pixel_size();
    pr4_planes_are_packed_without_row_padding_in_the_buffers_plane_order();
    fr26_a_portrait_short_goes_up_from_its_top_row();
    nfr5_planes_outside_the_mapping_are_not_read();
    pr4_a_picture_is_copied_once_and_again_only_when_it_changes();
    pr4_the_bytes_copied_are_reported_once_a_second();
    if (failures) {
        std::fprintf(stderr, "%d expectation(s) failed\n", failures);
        return 1;
    }
    std::printf("cpu upload: all tests passed\n");
    return 0;
}
