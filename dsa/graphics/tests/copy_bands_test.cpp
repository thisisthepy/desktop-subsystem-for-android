// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Large copies are cut into GPU jobs of a bounded size, so a maximized window's frame on a
// 200% monitor never makes one job that grows with the window.

#include <cstdint>
#include <cstdio>
#include <cstdlib>

#include "dsa_copy_bands.h"

namespace {

int failures = 0;

void check(bool ok, const char* test, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "%s: %s\n", test, what);
        failures++;
    }
}

/* Every row once, in order, and no band over the limit. */
void covers(const char* test, uint32_t width, uint32_t height, uint64_t max_pixels) {
    const auto bands = dsa::copy_bands(width, height, max_pixels);
    uint32_t next = 0;
    for (const auto& band : bands) {
        check(band.y == next, test, "bands follow each other");
        check(band.rows > 0, test, "no empty band");
        check(uint64_t(band.rows) * width <= max_pixels || band.rows == 1, test,
              "no band over the limit");
        next = band.y + band.rows;
    }
    check(next == height, test, "every row");
}

void nfr5_a_window_sized_frame_is_one_job() {
    const char* test = __func__;
    check(dsa::copy_bands(1920, 1080).size() == 1, test, "1920x1080 in one");
    check(dsa::copy_bands(1536, 1536).size() == 2, test, "1536x1536 in two");
    covers(test, 1048, 1278, dsa::kMaxCopyPixels);
}

void nfr5_a_maximized_frame_on_a_200_percent_monitor_is_cut_into_bounded_jobs() {
    const char* test = __func__;
    // The owner's window, maximized to 2880x1800 in a 2880x2048 ColorBuffer, just before
    // the GPU reset of 2026-11-12.
    const auto frame = dsa::copy_bands(2880, 1800);
    check(frame.size() == 3, test, "2880x1800 in three");
    covers(test, 2880, 1800, dsa::kMaxCopyPixels);
    covers(test, 2880, 2048, dsa::kMaxCopyPixels);
    covers(test, 3840, 2160, dsa::kMaxCopyPixels);
    covers(test, 7680, 4320, dsa::kMaxCopyPixels);
    // Equal bands, not two full ones and a sliver.
    check(frame[0].rows == 600 && frame[2].rows == 600, test, "equal bands");
}

void nfr5_odd_sizes_and_empty_copies() {
    const char* test = __func__;
    check(dsa::copy_bands(0, 100).empty() && dsa::copy_bands(100, 0).empty(), test, "empty");
    covers(test, 1, 1, dsa::kMaxCopyPixels);
    covers(test, 1000, 7, 1500);
    // A row wider than the limit is still copied, a row at a time.
    const auto wide = dsa::copy_bands(5000, 3, 1000);
    check(wide.size() == 3 && wide[0].rows == 1, test, "one row each");
    // A copy between buffers holds as many bytes as a band of RGBA pixels.
    check(dsa::kMaxCopyBytes == dsa::kMaxCopyPixels * 4, test, "bytes of a band");
}

}  // namespace

int main() {
    nfr5_a_window_sized_frame_is_one_job();
    nfr5_a_maximized_frame_on_a_200_percent_monitor_is_cut_into_bounded_jobs();
    nfr5_odd_sizes_and_empty_copies();
    if (failures) {
        return EXIT_FAILURE;
    }
    std::puts("copy_bands_test: ok");
    return EXIT_SUCCESS;
}
