// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The host ColorBuffer every guest process derives from one gralloc buffer: its handle and
// its format. The hwcomposer and the guest Vulkan driver both use these, and whichever
// creates the ColorBuffer first decides them for all.

#include <cstdint>

#include "dsa_shared_color_buffer.h"
#include "test_main.h"

namespace {

constexpr uint32_t fourcc(char a, char b, char c, char d) {
    return uint32_t(uint8_t(a)) | (uint32_t(uint8_t(b)) << 8) | (uint32_t(uint8_t(c)) << 16) |
           (uint32_t(uint8_t(d)) << 24);
}

constexpr uint32_t kGlRgba = 0x1908;

}  // namespace

TEST(fr3_shared_color_buffer_handle_keeps_apart_from_host_handles) {
    EXPECT_EQ(dsa_shared_color_buffer_handle(1), 0x80000001u);
    EXPECT_EQ(dsa_shared_color_buffer_handle(0x80000005u), 0x80000005u);
}

TEST(fr3_shared_color_buffer_has_the_format_of_its_gralloc_buffer) {
    // The guest Vulkan driver makes an imported buffer's VkImage in the buffer's format;
    // the ColorBuffer under it must be the same, or the image is bound to memory of
    // another size and layout.
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('A', 'B', '2', '4')), kGlRgba);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('X', 'B', '2', '4')), kGlRgba);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('A', 'R', '2', '4')), 0x80E1u);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('B', 'G', '2', '4')), 0x1907u);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('R', 'G', '1', '6')), 0x8D62u);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('A', 'B', '4', 'H')), 0x881Au);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('A', 'B', '3', '0')), 0x6AF9u);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('R', '8', ' ', ' ')), 0x8229u);
}

TEST(fr3_shared_color_buffer_without_an_equivalent_stays_rgba) {
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(fourcc('N', 'V', '1', '2')), kGlRgba);
    EXPECT_EQ(dsa_shared_color_buffer_gl_format(0), kGlRgba);
}

int main() { return ::dsa::test::runAll(); }
