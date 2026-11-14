// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// How the renderer cuts a large copy into GPU jobs of a bounded size.
//
// Windows resets a GPU whose work does not yield for two seconds, and an integrated GPU
// shares its time and memory bandwidth with the whole desktop. A frame copied into a
// maximized window on a 200% monitor is 2880x1800 pixels or more, in a ColorBuffer of up to
// 2880x2048. Each copy the renderer submits covers at most kMaxCopyPixels, in bands of whole
// rows, each in a submission of its own, so the GPU can switch to other work between them
// and no single job grows with the window.

#pragma once

#include <cstdint>
#include <vector>

namespace dsa {

// The most pixels one submitted copy covers: a 1920x1080 frame fits in one.
inline constexpr uint64_t kMaxCopyPixels = 2u * 1024u * 1024u;

// The most bytes one submitted copy between buffers covers: as many as kMaxCopyPixels of
// RGBA pixels.
inline constexpr uint64_t kMaxCopyBytes = kMaxCopyPixels * 4u;

// Rows `y` to `y + rows` of a copy.
struct CopyBand {
    uint32_t y = 0;
    uint32_t rows = 0;
};

// The bands a `width` x `height` copy is cut into: as few as fit `max_pixels` each, of equal
// height but the last, covering every row once and in order. At least one row each, so a
// row wider than `max_pixels` is still copied. Empty for an empty copy.
inline std::vector<CopyBand> copy_bands(uint32_t width, uint32_t height,
                                        uint64_t max_pixels = kMaxCopyPixels) {
    std::vector<CopyBand> bands;
    if (width == 0 || height == 0) {
        return bands;
    }
    uint64_t rows_per_band = max_pixels / width;
    if (rows_per_band == 0) {
        rows_per_band = 1;
    }
    const uint64_t count = (height + rows_per_band - 1) / rows_per_band;
    // Equal bands rather than full ones and a sliver: the same number of jobs, each smaller.
    const uint64_t rows = (height + count - 1) / count;
    for (uint64_t y = 0; y < height; y += rows) {
        const uint64_t left = height - y;
        bands.push_back(CopyBand{static_cast<uint32_t>(y),
                                 static_cast<uint32_t>(left < rows ? left : rows)});
    }
    return bands;
}

}  // namespace dsa
