// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Which layers the host composes, and when a layer's buffer holds a new picture that is
// copied up to the host.

#include <cstdint>

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

using dsa::hwc::hostComposes;
using dsa::hwc::LayerPictures;
using dsa::hwc::UploadFailureLog;

TEST(pr4_a_video_layer_is_drawn_by_surfaceflinger_not_the_host) {
    // The host's compositor samples RGB images only.
    EXPECT_TRUE(!hostComposes(fourcc('9', '9', '9', '7')));  // YV12, the decoders' pictures
    EXPECT_TRUE(!hostComposes(fourcc('Y', 'V', '1', '2')));
    EXPECT_TRUE(!hostComposes(fourcc('Y', 'U', '1', '2')));
    EXPECT_TRUE(!hostComposes(fourcc('N', 'V', '1', '2')));
    EXPECT_TRUE(!hostComposes(fourcc('N', 'V', '2', '1')));
    EXPECT_TRUE(hostComposes(fourcc('A', 'B', '2', '4')));
    EXPECT_TRUE(hostComposes(fourcc('R', 'G', '1', '6')));
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

int main() { return ::dsa::test::runAll(); }
