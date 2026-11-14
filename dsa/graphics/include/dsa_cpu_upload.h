// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Copying what a CPU wrote into a gralloc buffer up to the buffer's host ColorBuffer, for the
// GPU that samples it or the host that composes it.
//
// A Hyper-V VM has no virtio-gpu device, so a gralloc buffer has two copies: the guest memory
// minigbm allocated, which a CPU lock maps, and the host ColorBuffer every GPU use of the
// buffer goes to. On a virtio-gpu device the kernel moves a CPU's writes across when the lock
// ends. Here nothing did: a video decoder's pictures, a camera's frames and a layer an app
// draws in software never reached the host, and a video showed its sound with a green or
// black picture (the ColorBuffer's untouched memory read as YUV is green).
//
// The planes are now uploaded, packed one after the other as the host's image takes them,
// by the process that uses the buffer before the GPU or the host reads it: the hwcomposer,
// for every layer it is handed (SurfaceFlinger's own drawing included, since it hands every
// layer over first), and the guest Vulkan driver in any other process that samples one.
//
// The cost stays at one copy for each picture a CPU wrote. A buffer only the GPU draws into
// is never copied. A layer whose buffer changes is a new picture, since a queue hands a
// buffer back only once its producer has written it again; a buffer seen again is copied
// only when its fingerprint, a sample of every eighth row of each plane, has changed.
//
// The ColorBuffer must have the buffer's own layout (dsa_shared_color_buffer.h): RGB formats
// as they are, and YUV only from a host that makes YUV ColorBuffers.

#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <unordered_map>

namespace dsa {

// Android's gralloc usage bits (AHARDWAREBUFFER_USAGE_*), mirrored to keep this header free of
// platform headers.
inline constexpr uint64_t kUploadUsageCpuWriteMask = 0xF0ULL;
inline constexpr uint64_t kUploadUsageGpuColorOutput = 1ULL << 9;

// Whether a buffer with this usage gets what a CPU wrote copied up to the host: the CPU
// writes it and the GPU does not draw into it (that drawing is the host's already).
inline bool wantsCpuUpload(uint64_t usage) {
    return (usage & kUploadUsageCpuWriteMask) != 0 && (usage & kUploadUsageGpuColorOutput) == 0;
}

enum class UploadKind { None, Rgb, Yuv420ThreePlane, Yuv420TwoPlane };

// One plane as the host takes it: `rows` rows of `rowBytes` bytes, with no padding.
struct UploadPlane {
    uint32_t rowBytes = 0;
    uint32_t rows = 0;
};

// How a buffer's planes go up, and the rcUpdateColorBuffer call that carries them.
struct UploadLayout {
    UploadKind kind = UploadKind::None;
    uint32_t planeCount = 0;
    UploadPlane planes[3];
    uint32_t glFormat = 0;
    uint32_t glType = 0;
    int32_t callWidth = 0;
    int32_t callHeight = 0;

    bool valid() const { return kind != UploadKind::None; }
    size_t packedSize() const {
        size_t size = 0;
        for (uint32_t i = 0; i < planeCount; ++i) {
            size += static_cast<size_t>(planes[i].rowBytes) * planes[i].rows;
        }
        return size;
    }
};

namespace upload_gl {
inline constexpr uint32_t kRgba = 0x1908;
inline constexpr uint32_t kRgb = 0x1907;
inline constexpr uint32_t kLuminance = 0x1909;
inline constexpr uint32_t kUnsignedByte = 0x1401;
inline constexpr uint32_t kUnsignedShort565 = 0x8363;
}  // namespace upload_gl

inline constexpr uint32_t uploadFourcc(char a, char b, char c, char d) {
    return uint32_t(uint8_t(a)) | (uint32_t(uint8_t(b)) << 8) | (uint32_t(uint8_t(c)) << 16) |
           (uint32_t(uint8_t(d)) << 24);
}

// The upload of a `width` by `height` buffer of DRM format `drmFourcc`, or an invalid layout
// for one that is not uploaded. `hostHasYuv` says the host makes YUV ColorBuffers; without
// them, and for an odd size (dsa_shared_color_buffer_create_format), a YUV buffer's
// ColorBuffer is RGBA and its planes would not fit it, so it is not uploaded.
inline UploadLayout uploadLayout(uint32_t drmFourcc, uint32_t width, uint32_t height,
                                 bool hostHasYuv) {
    UploadLayout layout;
    if (width == 0 || height == 0 || width > 16384 || height > 16384) {
        return layout;
    }
    uint32_t rgbBytes = 0;
    switch (drmFourcc) {
        case uploadFourcc('A', 'B', '2', '4'):
        case uploadFourcc('X', 'B', '2', '4'):
        case uploadFourcc('A', 'R', '2', '4'):
        case uploadFourcc('X', 'R', '2', '4'):
            rgbBytes = 4;
            layout.glFormat = upload_gl::kRgba;
            layout.glType = upload_gl::kUnsignedByte;
            break;
        case uploadFourcc('B', 'G', '2', '4'):
            rgbBytes = 3;
            layout.glFormat = upload_gl::kRgb;
            layout.glType = upload_gl::kUnsignedByte;
            break;
        case uploadFourcc('R', 'G', '1', '6'):
            rgbBytes = 2;
            layout.glFormat = upload_gl::kRgb;
            layout.glType = upload_gl::kUnsignedShort565;
            break;
        case uploadFourcc('R', '8', ' ', ' '):
            rgbBytes = 1;
            layout.glFormat = upload_gl::kLuminance;
            layout.glType = upload_gl::kUnsignedByte;
            break;
        case uploadFourcc('9', '9', '9', '7'):
        case uploadFourcc('Y', 'V', '1', '2'):
        case uploadFourcc('Y', 'U', '1', '2'):
            layout.kind = UploadKind::Yuv420ThreePlane;
            break;
        case uploadFourcc('N', 'V', '1', '2'):
        case uploadFourcc('N', 'V', '2', '1'):
            layout.kind = UploadKind::Yuv420TwoPlane;
            break;
        default:
            return layout;
    }
    if (rgbBytes != 0) {
        layout.kind = UploadKind::Rgb;
        layout.planeCount = 1;
        layout.planes[0] = UploadPlane{width * rgbBytes, height};
        layout.callWidth = static_cast<int32_t>(width);
        layout.callHeight = static_cast<int32_t>(height);
        return layout;
    }
    if (!hostHasYuv || width % 2 != 0 || height % 2 != 0) {
        return UploadLayout{};
    }
    // As the host lays the planes out in its staging copy: Y at full size, then each chroma
    // plane at half width and height (a two-plane chroma row holds pairs).
    layout.planes[0] = UploadPlane{width, height};
    if (layout.kind == UploadKind::Yuv420ThreePlane) {
        layout.planeCount = 3;
        layout.planes[1] = UploadPlane{width / 2, height / 2};
        layout.planes[2] = UploadPlane{width / 2, height / 2};
    } else {
        layout.planeCount = 2;
        layout.planes[1] = UploadPlane{width, height / 2};
    }
    // The packed planes go as one row of bytes; the host checks the count against its image.
    layout.glFormat = upload_gl::kLuminance;
    layout.glType = upload_gl::kUnsignedByte;
    layout.callWidth = static_cast<int32_t>(layout.packedSize());
    layout.callHeight = 1;
    return layout;
}

// Where a plane starts in the buffer's mapping, and the bytes from one row to the next.
struct SourcePlane {
    uint64_t offset = 0;
    uint32_t stride = 0;
};

namespace upload_detail {

inline bool planeFits(const SourcePlane& source, const UploadPlane& plane, size_t mappedSize) {
    if (plane.rows == 0 || plane.rowBytes == 0) {
        return true;
    }
    if (source.stride < plane.rowBytes) {
        return false;
    }
    const uint64_t last = source.offset + uint64_t(source.stride) * (plane.rows - 1) + plane.rowBytes;
    return last <= mappedSize;
}

inline uint64_t mix(uint64_t hash, uint64_t word) {
    hash ^= word;
    hash = (hash << 29) | (hash >> 35);
    return hash * 0x9E3779B97F4A7C15ULL;
}

}  // namespace upload_detail

// Whether every plane the layout uploads lies inside the mapping.
inline bool sourceFits(const SourcePlane* sources, uint32_t sourceCount, size_t mappedSize,
                       const UploadLayout& layout) {
    if (!layout.valid() || sourceCount < layout.planeCount) {
        return false;
    }
    for (uint32_t i = 0; i < layout.planeCount; ++i) {
        if (!upload_detail::planeFits(sources[i], layout.planes[i], mappedSize)) {
            return false;
        }
    }
    return true;
}

// Copies the planes out of the mapping into `out`, which holds layout.packedSize() bytes.
// False, with nothing promised of `out`, when a plane lies outside the mapping.
inline bool packPlanes(const uint8_t* mapped, size_t mappedSize, const SourcePlane* sources,
                       uint32_t sourceCount, const UploadLayout& layout, uint8_t* out) {
    if (!sourceFits(sources, sourceCount, mappedSize, layout)) {
        return false;
    }
    for (uint32_t i = 0; i < layout.planeCount; ++i) {
        const UploadPlane& plane = layout.planes[i];
        const uint8_t* row = mapped + sources[i].offset;
        for (uint32_t r = 0; r < plane.rows; ++r) {
            std::memcpy(out, row, plane.rowBytes);
            out += plane.rowBytes;
            row += sources[i].stride;
        }
    }
    return true;
}

// Every eighth row of each plane, and its last, are what the fingerprint reads.
inline constexpr uint32_t kFingerprintRowStep = 8;

// A fingerprint of the planes' contents: never 0, which stands for "never uploaded". 0 when a
// plane lies outside the mapping.
inline uint64_t fingerprint(const uint8_t* mapped, size_t mappedSize, const SourcePlane* sources,
                            uint32_t sourceCount, const UploadLayout& layout) {
    if (!sourceFits(sources, sourceCount, mappedSize, layout)) {
        return 0;
    }
    uint64_t hash = 0xCBF29CE484222325ULL;
    for (uint32_t i = 0; i < layout.planeCount; ++i) {
        const UploadPlane& plane = layout.planes[i];
        for (uint32_t r = 0; r < plane.rows; ++r) {
            if (r % kFingerprintRowStep != 0 && r + 1 != plane.rows) {
                continue;
            }
            const uint8_t* row = mapped + sources[i].offset + uint64_t(sources[i].stride) * r;
            uint32_t at = 0;
            for (; at + 8 <= plane.rowBytes; at += 8) {
                uint64_t word;
                std::memcpy(&word, row + at, 8);
                hash = upload_detail::mix(hash, word);
            }
            uint64_t tail = 0;
            std::memcpy(&tail, row + at, plane.rowBytes - at);
            hash = upload_detail::mix(hash, tail ^ (uint64_t(r) << 32) ^ i);
        }
    }
    return hash == 0 ? 1 : hash;
}

// The fingerprint each buffer was last uploaded with, by buffer.
class UploadTracker {
   public:
    // Whether a buffer whose contents now have `print` needs uploading. `fresh` says the
    // buffer holds a new picture whatever its fingerprint (a layer's buffer changed).
    bool needsUpload(uint64_t buffer, uint64_t print, bool fresh) const {
        if (fresh || print == 0) {
            return true;
        }
        auto it = mPrints.find(buffer);
        return it == mPrints.end() || it->second != print;
    }
    void uploaded(uint64_t buffer, uint64_t print) { mPrints[buffer] = print; }
    void forget(uint64_t buffer) { mPrints.erase(buffer); }
    size_t size() const { return mPrints.size(); }

   private:
    std::unordered_map<uint64_t, uint64_t> mPrints;
};

// The bytes uploaded over each second, for a debug line.
class UploadRate {
   public:
    static constexpr uint64_t kWindowNs = 1000000000ULL;

    struct Report {
        uint64_t bytes = 0;
        uint32_t uploads = 0;
        uint64_t windowNs = 0;
    };

    // Counts an upload made at `nowNs`. Returns true, with the window's totals in `report`,
    // when a second or more has passed since the window began; a new window begins then.
    bool add(uint64_t nowNs, uint64_t bytes, Report* report) {
        if (mUploads == 0) {
            mStartNs = nowNs;
        }
        mBytes += bytes;
        ++mUploads;
        if (nowNs - mStartNs < kWindowNs) {
            return false;
        }
        report->bytes = mBytes;
        report->uploads = mUploads;
        report->windowNs = nowNs - mStartNs;
        mBytes = 0;
        mUploads = 0;
        return true;
    }

   private:
    uint64_t mStartNs = 0;
    uint64_t mBytes = 0;
    uint32_t mUploads = 0;
};

}  // namespace dsa
