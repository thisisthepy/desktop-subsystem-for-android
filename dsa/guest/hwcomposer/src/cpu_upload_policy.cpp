// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "cpu_upload_policy.h"

#include <atomic>
#include <cstdio>

#include "dsa_shared_color_buffer.h"

namespace dsa::hwc {

namespace {

// Android's Dataspace fields (android/hardware/graphics/common/Dataspace.aidl).
constexpr int32_t kStandardShift = 16;
constexpr int32_t kStandardMask = 63 << kStandardShift;
constexpr int32_t kTransferShift = 22;
constexpr int32_t kTransferMask = 31 << kTransferShift;
constexpr int32_t kRangeShift = 27;
constexpr int32_t kRangeMask = 7 << kRangeShift;

enum Standard : int32_t {
    kStandardUnspecified = 0,
    kStandardBt709 = 1,
    kStandardBt601_625 = 2,
    kStandardBt601_625Unadjusted = 3,
    kStandardBt601_525 = 4,
    kStandardBt601_525Unadjusted = 5,
    kStandardBt2020 = 6,
    kStandardBt2020ConstantLuminance = 7,
    kStandardBt470M = 8,
};
enum Transfer : int32_t {
    kTransferSt2084 = 7,
    kTransferHlg = 8,
};
enum Range : int32_t {
    kRangeUnspecified = 0,
    kRangeFull = 1,
    kRangeLimited = 2,
};

// The legacy dataspaces, from before the fields.
constexpr int32_t kLegacyJfif = 0x101;
constexpr int32_t kLegacyBt601_625 = 0x102;
constexpr int32_t kLegacyBt601_525 = 0x103;
constexpr int32_t kLegacyBt709 = 0x104;

constexpr uint32_t fourcc(char a, char b, char c, char d) {
    return uint32_t(uint8_t(a)) | (uint32_t(uint8_t(b)) << 8) | (uint32_t(uint8_t(c)) << 16) |
           (uint32_t(uint8_t(d)) << 24);
}

// The internal format a ColorBuffer gets when it is not YUV.
constexpr uint32_t kGlRgba = 0x1908;

std::atomic<bool> gHostComposesYuv{false};

}  // namespace

std::optional<YuvColor> yuvColorOf(int32_t dataspace, uint32_t height) {
    switch (dataspace) {
        case kLegacyJfif:
            return YuvColor{YuvMatrix::Bt601, YuvRange::Full};
        case kLegacyBt601_625:
        case kLegacyBt601_525:
            return YuvColor{YuvMatrix::Bt601, YuvRange::Limited};
        case kLegacyBt709:
            return YuvColor{YuvMatrix::Bt709, YuvRange::Limited};
        default:
            break;
    }
    const int32_t transfer = (dataspace & kTransferMask) >> kTransferShift;
    if (transfer == kTransferSt2084 || transfer == kTransferHlg) {
        return std::nullopt;
    }
    YuvColor color;
    switch ((dataspace & kStandardMask) >> kStandardShift) {
        case kStandardUnspecified:
            color.matrix = height >= 720 ? YuvMatrix::Bt709 : YuvMatrix::Bt601;
            break;
        case kStandardBt709:
            color.matrix = YuvMatrix::Bt709;
            break;
        case kStandardBt601_625:
        case kStandardBt601_625Unadjusted:
        case kStandardBt601_525:
        case kStandardBt601_525Unadjusted:
        case kStandardBt470M:
            color.matrix = YuvMatrix::Bt601;
            break;
        case kStandardBt2020:
            color.matrix = YuvMatrix::Bt2020;
            break;
        case kStandardBt2020ConstantLuminance:
        default:
            return std::nullopt;
    }
    switch ((dataspace & kRangeMask) >> kRangeShift) {
        case kRangeUnspecified:
        case kRangeLimited:
            color.range = YuvRange::Limited;
            break;
        case kRangeFull:
            color.range = YuvRange::Full;
            break;
        default:
            return std::nullopt;
    }
    return color;
}

bool chromaVuOrder(uint32_t drmFourcc) {
    switch (drmFourcc) {
        case fourcc('9', '9', '9', '7'):  // YV12 (YVU420_ANDROID): Y, V, U
        case fourcc('Y', 'V', '1', '2'):
        case fourcc('N', 'V', '2', '1'):  // V, U pairs
            return true;
        default:
            return false;
    }
}

void encodeYuvColor(const YuvColor& color, uint8_t out[4]) {
    out[0] = static_cast<uint8_t>(color.matrix);
    out[1] = static_cast<uint8_t>(color.range);
    out[2] = color.vuOrder ? 1 : 0;
    out[3] = kYuvColorMarker;
}

Rgb yuvToRgb(uint8_t y, uint8_t u, uint8_t v, const YuvColor& color) {
    // Kr and Kb of each matrix (ITU-R BT.601, BT.709, BT.2020 non-constant luminance).
    double kr = 0.299;
    double kb = 0.114;
    if (color.matrix == YuvMatrix::Bt709) {
        kr = 0.2126;
        kb = 0.0722;
    } else if (color.matrix == YuvMatrix::Bt2020) {
        kr = 0.2627;
        kb = 0.0593;
    }
    const double kg = 1.0 - kr - kb;
    double ey;
    double ecb;
    double ecr;
    if (color.range == YuvRange::Full) {
        ey = y / 255.0;
        ecb = (u - 128.0) / 255.0;
        ecr = (v - 128.0) / 255.0;
    } else {
        // Luma from 16 to 235, chroma from 16 to 240 about 128.
        ey = (y - 16.0) / 219.0;
        ecb = (u - 128.0) / 224.0;
        ecr = (v - 128.0) / 224.0;
    }
    const double r = ey + 2.0 * (1.0 - kr) * ecr;
    const double b = ey + 2.0 * (1.0 - kb) * ecb;
    const double g = (ey - kr * r - kb * b) / kg;
    return Rgb{r, g, b};
}

void setHostComposesYuv(bool composes) { gHostComposesYuv.store(composes); }

bool hostComposesYuv() { return gHostComposesYuv.load(); }

bool hostComposes(uint32_t drmFourcc, uint32_t width, uint32_t height, int32_t dataspace,
                  bool hostComposesYuv) {
    if (dsa_shared_color_buffer_yuv_layout(drmFourcc) == 0) {
        return true;
    }
    return hostComposesYuv &&
           dsa_shared_color_buffer_create_format(drmFourcc, width, height, 1) != kGlRgba &&
           yuvColorOf(dataspace, height).has_value();
}

bool LayerPictures::show(int64_t layer, uint32_t buffer) {
    auto [it, added] = mShown.emplace(layer, buffer);
    if (added) {
        return true;
    }
    const bool changed = it->second != buffer;
    it->second = buffer;
    return changed;
}

bool UploadFailureLog::shouldLog(uint32_t* count) {
    ++mCount;
    *count = mCount;
    return mCount == 1 || (mEvery > 0 && mCount % mEvery == 0);
}

bool UploadTally::uploaded(uint64_t nowNs, const dsa::UploadLayout& layout, std::string* line) {
    dsa::UploadRate::Report report;
    if (!mRate.add(nowNs, layout.packedSize(), &report)) {
        return false;
    }
    char text[256];
    const int written = std::snprintf(
        text, sizeof(text),
        "copied %llu KiB that a CPU wrote up to the host in %u uploads over %llu ms",
        static_cast<unsigned long long>(report.bytes / 1024), report.uploads,
        static_cast<unsigned long long>(report.windowNs / 1000000));
    if ((mNoPicture != 0 || mChanged != 0) && written > 0 &&
        static_cast<size_t>(written) < sizeof(text)) {
        std::snprintf(text + written, sizeof(text) - static_cast<size_t>(written),
                      "; kept from the host: %u that held no picture (zeros), %u that changed "
                      "before the host read them",
                      mNoPicture, mChanged);
    }
    *line = text;
    mNoPicture = 0;
    mChanged = 0;
    return true;
}

}  // namespace dsa::hwc
