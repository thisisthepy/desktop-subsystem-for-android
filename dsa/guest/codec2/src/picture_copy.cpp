// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "picture_copy.h"

#include <cstring>

namespace dsa::codec2 {

bool copyNv12(const uint8_t* nv12, size_t size, uint32_t width, uint32_t height, uint32_t stride,
              const YuvPlanes& planes) {
    const size_t chromaRows = (height + 1) / 2;
    const size_t chromaWidth = (width + 1) / 2;
    // Each chroma row holds whole Cb, Cr pairs.
    if (width == 0 || height == 0 || stride < chromaWidth * 2) {
        return false;
    }
    if (size < static_cast<size_t>(stride) * (height + chromaRows)) {
        return false;
    }
    for (uint32_t row = 0; row < height; ++row) {
        const uint8_t* src = nv12 + static_cast<size_t>(row) * stride;
        uint8_t* dst = planes.y.data + static_cast<size_t>(row) * planes.y.rowStride;
        if (planes.y.pixelStride == 1) {
            std::memcpy(dst, src, width);
        } else {
            for (uint32_t x = 0; x < width; ++x) {
                dst[x * planes.y.pixelStride] = src[x];
            }
        }
    }
    const uint8_t* chroma = nv12 + static_cast<size_t>(stride) * height;
    // Already NV12 (Cb then Cr interleaved): whole rows at once.
    const bool interleavedCbCr = planes.u.pixelStride == 2 && planes.v.pixelStride == 2 &&
                                 planes.v.data == planes.u.data + 1 && planes.u.rowStride == planes.v.rowStride;
    for (size_t row = 0; row < chromaRows; ++row) {
        const uint8_t* src = chroma + row * stride;
        uint8_t* u = planes.u.data + row * planes.u.rowStride;
        uint8_t* v = planes.v.data + row * planes.v.rowStride;
        if (interleavedCbCr) {
            std::memcpy(u, src, chromaWidth * 2);
            continue;
        }
        for (size_t x = 0; x < chromaWidth; ++x) {
            u[x * planes.u.pixelStride] = src[2 * x];
            v[x * planes.v.pixelStride] = src[2 * x + 1];
        }
    }
    return true;
}

}  // namespace dsa::codec2
