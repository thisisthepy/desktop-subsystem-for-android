// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// When the host renderer composes a YUV ColorBuffer, and which uploads it refuses
// (dsa_yuv_pictures.h, patch 0045), run on the build machine.

#include "dsa_yuv_pictures.h"

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

using dsa::YuvPicture;

// Packed planes as the guest sends them: Y at full size, then the chroma at half height.
static std::vector<uint8_t> nv12(uint32_t width, uint32_t height, uint8_t luma, uint8_t chroma) {
    std::vector<uint8_t> planes(size_t(width) * height * 3 / 2, chroma);
    std::fill(planes.begin(), planes.begin() + size_t(width) * height, luma);
    return planes;
}

static void fr26_a_yuv_color_buffer_no_upload_finished_in_is_never_composed() {
    // A new ColorBuffer, or one made again on a new device after a GPU reset, holds its
    // allocation's zeros: solid green as YUV.
    EXPECT(!dsa::yuvPictureComposable(YuvPicture::Unwritten));
    EXPECT(!dsa::yuvPictureComposable(YuvPicture::Partial));
    EXPECT(dsa::yuvPictureComposable(YuvPicture::Whole));
}

static void fr26_only_an_upload_whose_every_job_finished_leaves_a_whole_picture() {
    // NV12 goes up in two jobs, one a plane.
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Unwritten, 2, 2, 2) == YuvPicture::Whole);
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Partial, 2, 2, 2) == YuvPicture::Whole);
    // The luma landed and the chroma job failed: new luma over zero or old chroma.
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Whole, 2, 1, 1) == YuvPicture::Partial);
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Unwritten, 2, 2, 1) == YuvPicture::Partial);
    // The last job reached the GPU but its fence was never seen: nothing says it finished.
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Whole, 2, 2, 1) == YuvPicture::Partial);
}

static void fr26_an_upload_that_reached_no_gpu_leaves_the_picture_it_found() {
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Whole, 2, 0, 0) == YuvPicture::Whole);
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Unwritten, 3, 0, 0) == YuvPicture::Unwritten);
    EXPECT(dsa::yuvPictureAfterUpload(YuvPicture::Partial, 2, 0, 0) == YuvPicture::Partial);
}

static void fr26_planes_no_producer_wrote_are_refused() {
    // A Short's 854x480 pictures, zeros as a buffer holds them before its decoder writes it.
    const std::vector<uint8_t> zeros = nv12(854, 480, 0, 0);
    EXPECT(dsa::yuvUploadHoldsNoPicture(false, 854, 480, zeros.data(), zeros.size()));
    // Three planes too.
    const std::vector<uint8_t> yv12(size_t(854) * 480 * 3 / 2, 0);
    EXPECT(dsa::yuvUploadHoldsNoPicture(true, 854, 480, yv12.data(), yv12.size()));
}

static void fr26_a_picture_written_only_in_part_is_refused() {
    // The decoder had written the top rows when another process copied the buffer up.
    std::vector<uint8_t> planes = nv12(854, 480, 0, 0);
    std::fill(planes.begin(), planes.begin() + 854 * 200, 90);
    EXPECT(dsa::yuvUploadHoldsNoPicture(false, 854, 480, planes.data(), planes.size()));
}

static void fr26_black_and_dark_pictures_go_up() {
    // Black is chroma 128 in either range, and luma 16 in the limited one.
    const std::vector<uint8_t> limited = nv12(854, 480, 16, 128);
    const std::vector<uint8_t> full = nv12(854, 480, 0, 128);
    const std::vector<uint8_t> dark = nv12(854, 480, 1, 0);
    EXPECT(!dsa::yuvUploadHoldsNoPicture(false, 854, 480, limited.data(), limited.size()));
    EXPECT(!dsa::yuvUploadHoldsNoPicture(false, 854, 480, full.data(), full.size()));
    EXPECT(!dsa::yuvUploadHoldsNoPicture(false, 854, 480, dark.data(), dark.size()));
}

static void fr26_planes_of_another_size_are_left_to_the_size_check() {
    const std::vector<uint8_t> zeros = nv12(854, 480, 0, 0);
    EXPECT(!dsa::yuvUploadHoldsNoPicture(false, 1080, 1920, zeros.data(), zeros.size()));
    EXPECT(!dsa::yuvUploadHoldsNoPicture(false, 854, 480, nullptr, zeros.size()));
}

static void fr26_the_log_says_at_once_then_at_most_every_five_seconds() {
    dsa::YuvPictureTally tally;
    std::string line;
    constexpr uint64_t kSecond = 1000000000ULL;
    EXPECT(tally.compositionHeld(YuvPicture::Unwritten, 10 * kSecond, &line));
    EXPECT(line.find("1 composition(s)") != std::string::npos);
    EXPECT(line.find("1 never written, 0 left part-written") != std::string::npos);

    line.clear();
    EXPECT(!tally.compositionHeld(YuvPicture::Partial, 11 * kSecond, &line));
    EXPECT(!tally.uploadRefused(12 * kSecond, &line));
    EXPECT(line.empty());

    // Past the interval, the line counts what happened since the last one.
    EXPECT(tally.uploadRefused(15 * kSecond, &line));
    EXPECT(line.find("1 composition(s)") != std::string::npos);
    EXPECT(line.find("0 never written, 1 left part-written") != std::string::npos);
    EXPECT(line.find("2 upload(s)") != std::string::npos);
    EXPECT(line.find("2 and 2 since the renderer started") != std::string::npos);
    EXPECT(tally.totalHeld() == 2);
    EXPECT(tally.totalRefused() == 2);
}

static void fr26_the_log_line_names_no_planning_document() {
    dsa::YuvPictureTally tally;
    std::string line;
    EXPECT(tally.uploadRefused(1, &line));
    EXPECT(line.find("FR-") == std::string::npos);
    EXPECT(line.find("SPEC") == std::string::npos);
}

int main() {
    fr26_a_yuv_color_buffer_no_upload_finished_in_is_never_composed();
    fr26_only_an_upload_whose_every_job_finished_leaves_a_whole_picture();
    fr26_an_upload_that_reached_no_gpu_leaves_the_picture_it_found();
    fr26_planes_no_producer_wrote_are_refused();
    fr26_a_picture_written_only_in_part_is_refused();
    fr26_black_and_dark_pictures_go_up();
    fr26_planes_of_another_size_are_left_to_the_size_check();
    fr26_the_log_says_at_once_then_at_most_every_five_seconds();
    fr26_the_log_line_names_no_planning_document();
    if (failures != 0) {
        std::fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    std::printf("yuv_pictures_test: all passed\n");
    return 0;
}
