// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Which layers the host composes, and when a layer's buffer holds a new picture that is
// copied up to the host.

#include <cmath>
#include <cstddef>
#include <cstdint>
#include <string>

#include "cpu_upload_policy.h"
#include "dsa_hardware_buffer_import.h"
#include "dsa_shared_color_buffer.h"
#include "test_main.h"

namespace {

constexpr uint32_t fourcc(char a, char b, char c, char d) {
    return uint32_t(uint8_t(a)) | (uint32_t(uint8_t(b)) << 8) | (uint32_t(uint8_t(c)) << 16) |
           (uint32_t(uint8_t(d)) << 24);
}

}  // namespace

using dsa::hwc::chromaVuOrder;
using dsa::hwc::encodeYuvColor;
using dsa::hwc::hostComposes;
using dsa::hwc::kYuvColorMarker;
using dsa::hwc::LayerPictures;
using dsa::hwc::Rgb;
using dsa::hwc::UploadFailureLog;
using dsa::hwc::YuvColor;
using dsa::hwc::yuvColorOf;
using dsa::hwc::YuvMatrix;
using dsa::hwc::YuvRange;
using dsa::hwc::yuvToRgb;

namespace {

// Android Dataspace values (android/hardware/graphics/common/Dataspace.aidl).
constexpr int32_t kUnknown = 0;
constexpr int32_t kBt709 = 0x10C10000;       // BT709, SMPTE_170M transfer, limited range
constexpr int32_t kBt601_625 = 0x10C20000;   // BT601_625, SMPTE_170M, limited
constexpr int32_t kBt601_525 = 0x10C40000;   // BT601_525, SMPTE_170M, limited
constexpr int32_t kJfif = 0x08C20000;        // BT601_625, SMPTE_170M, full
constexpr int32_t kBt2020 = 0x10C60000;      // BT2020, SMPTE_170M, limited
constexpr int32_t kBt2020Pq = 0x11C60000;    // BT2020, ST2084, limited (HDR10)
constexpr int32_t kBt2020Hlg = 0x12060000;   // BT2020, HLG, limited
constexpr int32_t kBt2020Cl = 0x10C70000;    // BT2020 constant luminance
constexpr int32_t kDisplayP3 = 0x088A0000;   // DCI_P3, SRGB, full
constexpr int32_t kExtended = 0x18C10000;    // BT709, extended range
constexpr int32_t kLegacyJfif = 0x101;
constexpr int32_t kLegacyBt709 = 0x104;

bool near(double a, double b) { return std::fabs(a - b) < 0.002; }

bool same(const std::optional<YuvColor>& got, YuvMatrix matrix, YuvRange range) {
    return got.has_value() && got->matrix == matrix && got->range == range;
}

}  // namespace

TEST(nfr1_a_video_layer_is_composed_by_a_host_that_composes_yuv) {
    // So SurfaceFlinger draws nothing into its full-size client target for a playing video.
    for (uint32_t format : {fourcc('9', '9', '9', '7'), fourcc('Y', 'V', '1', '2'),
                            fourcc('Y', 'U', '1', '2'), fourcc('N', 'V', '1', '2'),
                            fourcc('N', 'V', '2', '1')}) {
        EXPECT_TRUE(hostComposes(format, 1920, 1080, kBt709, true));
        EXPECT_TRUE(hostComposes(format, 640, 360, kUnknown, true));
    }
}

TEST(pr3_a_video_layer_falls_back_to_surfaceflinger_where_the_host_cannot_show_it) {
    const uint32_t nv12 = fourcc('N', 'V', '1', '2');
    // A host that does not compose YUV, as before.
    EXPECT_TRUE(!hostComposes(nv12, 1920, 1080, kBt709, false));
    // An odd size has an RGBA ColorBuffer, not a YUV one.
    EXPECT_TRUE(!hostComposes(nv12, 1921, 1080, kBt709, true));
    EXPECT_TRUE(!hostComposes(nv12, 1920, 1081, kBt709, true));
    // HDR needs tone mapping; constant luminance, extended range and RGB standards have no
    // YCbCr conversion.
    EXPECT_TRUE(!hostComposes(nv12, 3840, 2160, kBt2020Pq, true));
    EXPECT_TRUE(!hostComposes(nv12, 3840, 2160, kBt2020Hlg, true));
    EXPECT_TRUE(!hostComposes(nv12, 3840, 2160, kBt2020Cl, true));
    EXPECT_TRUE(!hostComposes(nv12, 1920, 1080, kExtended, true));
    EXPECT_TRUE(!hostComposes(nv12, 1920, 1080, kDisplayP3, true));
    // RGB layers are the host's whatever it says about YUV.
    EXPECT_TRUE(hostComposes(fourcc('A', 'B', '2', '4'), 427, 241, kUnknown, false));
    EXPECT_TRUE(hostComposes(fourcc('R', 'G', '1', '6'), 427, 241, kDisplayP3, false));
}

TEST(pr3_a_video_layers_matrix_and_range_follow_its_dataspace) {
    EXPECT_TRUE(same(yuvColorOf(kBt709, 1080), YuvMatrix::Bt709, YuvRange::Limited));
    EXPECT_TRUE(same(yuvColorOf(kBt601_625, 1080), YuvMatrix::Bt601, YuvRange::Limited));
    EXPECT_TRUE(same(yuvColorOf(kBt601_525, 480), YuvMatrix::Bt601, YuvRange::Limited));
    EXPECT_TRUE(same(yuvColorOf(kJfif, 480), YuvMatrix::Bt601, YuvRange::Full));
    EXPECT_TRUE(same(yuvColorOf(kBt2020, 2160), YuvMatrix::Bt2020, YuvRange::Limited));
    EXPECT_TRUE(same(yuvColorOf(kLegacyJfif, 480), YuvMatrix::Bt601, YuvRange::Full));
    EXPECT_TRUE(same(yuvColorOf(kLegacyBt709, 480), YuvMatrix::Bt709, YuvRange::Limited));
    // Without a description: HD is BT.709 and SD BT.601, both limited, as players assume.
    EXPECT_TRUE(same(yuvColorOf(kUnknown, 720), YuvMatrix::Bt709, YuvRange::Limited));
    EXPECT_TRUE(same(yuvColorOf(kUnknown, 576), YuvMatrix::Bt601, YuvRange::Limited));
    EXPECT_TRUE(!yuvColorOf(kBt2020Pq, 2160).has_value());
}

TEST(pr3_a_video_layers_chroma_order_follows_its_format) {
    EXPECT_TRUE(chromaVuOrder(fourcc('9', '9', '9', '7')));  // YV12: Y, V, U
    EXPECT_TRUE(chromaVuOrder(fourcc('Y', 'V', '1', '2')));
    EXPECT_TRUE(chromaVuOrder(fourcc('N', 'V', '2', '1')));
    EXPECT_TRUE(!chromaVuOrder(fourcc('Y', 'U', '1', '2')));  // I420: Y, U, V
    EXPECT_TRUE(!chromaVuOrder(fourcc('N', 'V', '1', '2')));
}

TEST(pr3_a_video_layers_colour_description_is_what_the_host_reads) {
    // Red the matrix, green the range, blue the order, alpha the marker: the host's reading.
    uint8_t color[4] = {};
    encodeYuvColor(YuvColor{YuvMatrix::Bt709, YuvRange::Limited, true}, color);
    EXPECT_EQ(color[0], 2);
    EXPECT_EQ(color[1], 2);
    EXPECT_EQ(color[2], 1);
    EXPECT_EQ(color[3], kYuvColorMarker);
    EXPECT_EQ(kYuvColorMarker, 0x59);
    encodeYuvColor(YuvColor{YuvMatrix::Bt601, YuvRange::Full, false}, color);
    EXPECT_EQ(color[0], 1);
    EXPECT_EQ(color[1], 1);
    EXPECT_EQ(color[2], 0);
    encodeYuvColor(YuvColor{YuvMatrix::Bt2020, YuvRange::Limited, false}, color);
    EXPECT_EQ(color[0], 3);
}

TEST(pr3_limited_range_black_and_white_stay_black_and_white) {
    for (YuvMatrix matrix : {YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020}) {
        const YuvColor limited{matrix, YuvRange::Limited, false};
        const Rgb black = yuvToRgb(16, 128, 128, limited);
        const Rgb white = yuvToRgb(235, 128, 128, limited);
        EXPECT_TRUE(near(black.r, 0) && near(black.g, 0) && near(black.b, 0));
        EXPECT_TRUE(near(white.r, 1) && near(white.g, 1) && near(white.b, 1));
        const YuvColor full{matrix, YuvRange::Full, false};
        const Rgb fullWhite = yuvToRgb(255, 128, 128, full);
        EXPECT_TRUE(near(fullWhite.r, 1) && near(fullWhite.g, 1) && near(fullWhite.b, 1));
    }
}

TEST(pr3_bt601_and_bt709_turn_colour_bars_into_their_primaries) {
    // 75% red of the ITU-R BT.801 / SMPTE bars in limited range: R 180, G 16, B 16 on 8 bits.
    const Rgb red601 = yuvToRgb(65, 100, 212, YuvColor{YuvMatrix::Bt601, YuvRange::Limited});
    EXPECT_TRUE(std::fabs(red601.r - 0.75) < 0.01);
    EXPECT_TRUE(std::fabs(red601.g) < 0.01 && std::fabs(red601.b) < 0.01);
    // The same red in BT.709 (Y 51, Cb 109, Cr 212).
    const Rgb red709 = yuvToRgb(51, 109, 212, YuvColor{YuvMatrix::Bt709, YuvRange::Limited});
    EXPECT_TRUE(std::fabs(red709.r - 0.75) < 0.01);
    EXPECT_TRUE(std::fabs(red709.g) < 0.01 && std::fabs(red709.b) < 0.01);
    // Read with the wrong matrix it is visibly off: why the dataspace decides the matrix.
    const Rgb wrong = yuvToRgb(51, 109, 212, YuvColor{YuvMatrix::Bt601, YuvRange::Limited});
    EXPECT_TRUE(std::fabs(wrong.g) > 0.03);
}

TEST(pr4_a_layer_whose_buffer_changes_holds_a_new_picture) {
    LayerPictures pictures;
    EXPECT_TRUE(pictures.show(1, 10));   // first seen
    EXPECT_TRUE(!pictures.show(1, 10));  // shown again
    EXPECT_TRUE(pictures.show(1, 11));   // the queue's next buffer
    EXPECT_TRUE(pictures.show(1, 10));   // back to the first, written again
    EXPECT_TRUE(pictures.show(2, 10));   // another layer keeps its own
    pictures.forget(1);
    EXPECT_TRUE(pictures.show(1, 10));
}

TEST(pr4_a_yuv_buffer_gets_a_yuv_color_buffer_only_from_a_host_that_makes_them) {
    const uint32_t yv12 = fourcc('9', '9', '9', '7');
    EXPECT_EQ(dsa_shared_color_buffer_create_format(yv12, 426, 240, 1),
              DSA_COLOR_BUFFER_YUV420_THREE_PLANE);
    EXPECT_EQ(dsa_shared_color_buffer_create_format(fourcc('N', 'V', '1', '2'), 360, 640, 1),
              DSA_COLOR_BUFFER_YUV420_TWO_PLANE);
    // A host without them, and a size a 4:2:0 image cannot have, keep RGBA as before.
    EXPECT_EQ(dsa_shared_color_buffer_create_format(yv12, 426, 240, 0), 0x1908u);
    EXPECT_EQ(dsa_shared_color_buffer_create_format(yv12, 427, 240, 1), 0x1908u);
    EXPECT_EQ(dsa_shared_color_buffer_create_format(yv12, 426, 241, 1), 0x1908u);
    // RGB buffers are unchanged.
    EXPECT_EQ(dsa_shared_color_buffer_create_format(fourcc('A', 'B', '2', '4'), 427, 241, 1),
              0x1908u);
    EXPECT_EQ(dsa_shared_color_buffer_create_format(fourcc('R', 'G', '1', '6'), 64, 64, 1),
              0x8D62u);
}

TEST(fr26_a_decoders_picture_buffer_is_imported_with_the_platforms_clone_method) {
    // AHardwareBuffer_createFromHandle refuses any method but REGISTER (2) and CLONE (3) with
    // BAD_VALUE before the mapper sees the buffer. A 1 here failed every import, so no picture
    // a decoder wrote reached the host and every video was green.
    EXPECT_EQ(dsa::kAhbCreateFromHandleRegister, 2);
    EXPECT_EQ(dsa::kAhbCreateFromHandleClone, 3);

    // A 640x360 YV12 picture from the codec HAL, as minigbm describes it: the mapper compares
    // width, height, requested format, layers and stride with these, and refuses a mismatch.
    dsa::GrallocBufferFacts facts;
    facts.width = 640;
    facts.height = 360;
    facts.androidFormat = 0x32315659;  // YV12
    facts.usage = 0x930;               // CPU_WRITE_OFTEN | GPU_SAMPLED_IMAGE | COMPOSER_OVERLAY
    facts.pixelStride = 640;
    const dsa::HardwareBufferImport import = dsa::cloneImportFor(facts);
    EXPECT_EQ(import.method, dsa::kAhbCreateFromHandleClone);
    EXPECT_EQ(import.width, 640u);
    EXPECT_EQ(import.height, 360u);
    EXPECT_EQ(import.layers, 1u);
    EXPECT_EQ(import.format, 0x32315659u);
    EXPECT_EQ(import.usage, 0x930ull);
    EXPECT_EQ(import.stride, 640u);

    // A row padded past the picture keeps the allocation's stride, not the width.
    facts.width = 426;
    facts.pixelStride = 432;
    EXPECT_EQ(dsa::cloneImportFor(facts).stride, 432u);
}

TEST(fr26_a_copy_up_that_fails_on_every_picture_keeps_being_logged) {
    UploadFailureLog log(3);
    uint32_t count = 0;
    EXPECT_TRUE(log.shouldLog(&count));  // the first of a run
    EXPECT_EQ(count, 1u);
    EXPECT_TRUE(!log.shouldLog(&count));
    EXPECT_TRUE(log.shouldLog(&count));  // again with the count, not silent for the boot
    EXPECT_EQ(count, 3u);
    log.reset();
    EXPECT_TRUE(log.shouldLog(&count));  // a new run after a success
    EXPECT_EQ(count, 1u);
}

// fr26: the upload line counts the planes' packed size. It counted the stream's own staging
// copy, which is empty when the planes go through the graphics memory the host shares, so a
// 1080x1920 Short's 30 copies a second read "copied 0 KiB".
TEST(fr26_the_upload_line_counts_what_reached_the_host_on_either_path) {
    const dsa::UploadLayout layout =
        dsa::uploadLayout(fourcc('N', 'V', '1', '2'), 1080, 1920, true);
    EXPECT_EQ(layout.packedSize(), size_t(1080) * 1920 * 3 / 2);
    dsa::hwc::UploadTally tally;
    std::string line;
    EXPECT_TRUE(!tally.uploaded(0, layout, &line));
    EXPECT_TRUE(!tally.uploaded(500000000, layout, &line));
    EXPECT_TRUE(tally.uploaded(1000000000, layout, &line));
    // 3 x 3110400 bytes.
    EXPECT_TRUE(line.find("copied 9112 KiB") != std::string::npos);
    EXPECT_TRUE(line.find("in 3 uploads over 1000 ms") != std::string::npos);
    EXPECT_TRUE(line.find("kept from the host") == std::string::npos);
}

// fr26: a picture kept from the host (zeros, or changed while the host read it) is said in the
// next line, so a green frame can be traced to where its zeros came from.
TEST(fr26_the_upload_line_says_which_pictures_were_kept_from_the_host) {
    const dsa::UploadLayout layout = dsa::uploadLayout(fourcc('N', 'V', '1', '2'), 720, 1280, true);
    dsa::hwc::UploadTally tally;
    std::string line;
    tally.heldNoPicture();
    tally.changedUnderHost();
    tally.changedUnderHost();
    EXPECT_TRUE(!tally.uploaded(0, layout, &line));
    EXPECT_TRUE(tally.uploaded(2000000000, layout, &line));
    EXPECT_TRUE(line.find("1 that held no picture (zeros), 2 that changed") != std::string::npos);
    // The next window starts its counts again.
    EXPECT_TRUE(!tally.uploaded(3000000000ULL, layout, &line));
    EXPECT_TRUE(tally.uploaded(4000000000ULL, layout, &line));
    EXPECT_TRUE(line.find("kept from the host") == std::string::npos);
}

int main() { return ::dsa::test::runAll(); }
