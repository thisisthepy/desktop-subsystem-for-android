// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "stream_config.h"

#include <algorithm>
#include <set>

namespace dsa::camera {

namespace {

constexpr int64_t kSecondNs = 1000000000;

uint64_t area(uint32_t width, uint32_t height) { return static_cast<uint64_t>(width) * height; }

const StreamSize* find(const std::vector<StreamSize>& sizes, uint32_t width, uint32_t height) {
    for (const StreamSize& size : sizes) {
        if (size.width == width && size.height == height) {
            return &size;
        }
    }
    return nullptr;
}

// The sizes apps ask for without looking (CameraX's analysis, preview and recording defaults
// among them), which a camera offers whenever one of its modes is at least as large: the
// picture of that mode is cropped to the size's shape at the centre and scaled down.
constexpr std::pair<uint32_t, uint32_t> kCommonSizes[] = {
    {1920, 1080}, {1280, 960}, {1280, 720}, {800, 600}, {720, 480},
    {640, 480},   {640, 360},  {352, 288},  {320, 240}, {176, 144},
};

bool sameShape(uint32_t w1, uint32_t h1, uint32_t w2, uint32_t h2) {
    // Within 1%: 1920x1080 and 1280x720 are both 16:9.
    const uint64_t a = static_cast<uint64_t>(w1) * h2;
    const uint64_t b = static_cast<uint64_t>(w2) * h1;
    return (a > b ? a - b : b - a) * 100 <= std::max(a, b);
}

}  // namespace

std::vector<StreamSize> modeSizes(const HostCamera& camera) {
    std::vector<StreamSize> sizes;
    for (const Mode& mode : camera.modes) {
        if (mode.width == 0 || mode.height == 0 || mode.maxFps == 0) {
            continue;
        }
        const int64_t duration = kSecondNs / mode.maxFps;
        auto known = std::find_if(sizes.begin(), sizes.end(), [&](const StreamSize& s) {
            return s.width == mode.width && s.height == mode.height;
        });
        if (known != sizes.end()) {
            known->minFrameDurationNs = std::min(known->minFrameDurationNs, duration);
            continue;
        }
        sizes.push_back(StreamSize{mode.width, mode.height, duration});
    }
    std::sort(sizes.begin(), sizes.end(), [](const StreamSize& a, const StreamSize& b) {
        if (area(a.width, a.height) != area(b.width, b.height)) {
            return area(a.width, a.height) > area(b.width, b.height);
        }
        return a.width > b.width;
    });
    return sizes;
}

std::vector<StreamSize> streamSizes(const HostCamera& camera) {
    const std::vector<StreamSize> modes = modeSizes(camera);
    std::vector<StreamSize> sizes = modes;
    for (const auto& [width, height] : kCommonSizes) {
        if (find(sizes, width, height) != nullptr) {
            continue;
        }
        // As fast as the fastest mode it is cut from.
        int64_t duration = 0;
        for (const StreamSize& mode : modes) {
            if (mode.width >= width && mode.height >= height &&
                (duration == 0 || mode.minFrameDurationNs < duration)) {
                duration = mode.minFrameDurationNs;
            }
        }
        if (duration != 0) {
            sizes.push_back(StreamSize{width, height, duration});
        }
    }
    std::sort(sizes.begin(), sizes.end(), [](const StreamSize& a, const StreamSize& b) {
        if (area(a.width, a.height) != area(b.width, b.height)) {
            return area(a.width, a.height) > area(b.width, b.height);
        }
        return a.width > b.width;
    });
    return sizes;
}

int32_t overrideFormatFor(int32_t format, uint64_t consumerUsage) {
    if (format != kFormatImplementationDefined) {
        return format;
    }
    // The guest's allocator makes no YUV buffer the composer may show, so a stream the
    // composer shows (a SurfaceView) gets RGBA, even when a video encoder shares it: the
    // allocator drops the encoder's bit for RGBA but never adds the composer's to YUV. A
    // stream the GPU samples (a TextureView) gets RGBA too, unless it goes to an encoder,
    // which takes YUV.
    if ((consumerUsage & kUsageComposerOverlay) != 0) {
        return kFormatRgba8888;
    }
    if ((consumerUsage & kUsageVideoEncoder) == 0 &&
        (consumerUsage & (kUsageGpuTexture | kUsageGpuRenderTarget)) != 0) {
        return kFormatRgba8888;
    }
    return kFormatYcbcr420;
}

uint64_t producerUsageFor(int32_t format) {
    if (format == kFormatRgba8888) {
        return kUsageCpuWriteOften;
    }
    return kUsageCpuWriteOften | kUsageCameraOutput;
}

std::vector<std::pair<int32_t, int32_t>> fpsRanges(const HostCamera& camera) {
    std::set<std::pair<int32_t, int32_t>> ranges;
    for (const Mode& mode : camera.modes) {
        if (mode.maxFps == 0) {
            continue;
        }
        const int32_t high = static_cast<int32_t>(mode.maxFps);
        const int32_t low = static_cast<int32_t>(std::min(mode.minFps == 0 ? mode.maxFps : mode.minFps, mode.maxFps));
        ranges.insert({high, high});
        ranges.insert({low, high});
    }
    return std::vector<std::pair<int32_t, int32_t>>(ranges.begin(), ranges.end());
}

std::pair<int32_t, int32_t> targetFpsRange(const HostCamera& camera, bool steady) {
    std::vector<std::pair<int32_t, int32_t>> ranges = fpsRanges(camera);
    if (ranges.empty()) {
        return {15, 30};
    }
    std::vector<std::pair<int32_t, int32_t>> upTo30;
    for (const auto& range : ranges) {
        if (range.second <= 30) {
            upTo30.push_back(range);
        }
    }
    if (!upTo30.empty()) {
        ranges = upTo30;
    }
    // Ranked by the highest rate, then: for video a fixed rate, else the lowest floor.
    const auto better = [steady](const std::pair<int32_t, int32_t>& a, const std::pair<int32_t, int32_t>& b) {
        if (a.second != b.second) {
            return a.second > b.second;
        }
        if (steady) {
            return a.first > b.first;
        }
        return a.first < b.first;
    };
    std::pair<int32_t, int32_t> best = ranges.front();
    for (const auto& range : ranges) {
        if (better(range, best)) {
            best = range;
        }
    }
    return best;
}

StreamSize largest(const HostCamera& camera) {
    const std::vector<StreamSize> sizes = streamSizes(camera);
    return sizes.empty() ? StreamSize{} : sizes.front();
}

bool streamsSupported(const HostCamera& camera, const std::vector<StreamAsk>& streams, std::string* why) {
    const std::vector<StreamSize> sizes = streamSizes(camera);
    int processed = 0;
    int stills = 0;
    if (streams.empty()) {
        *why = "no stream";
        return false;
    }
    for (const StreamAsk& stream : streams) {
        if (!stream.output) {
            *why = "the camera takes no input stream";
            return false;
        }
        if (find(sizes, stream.width, stream.height) == nullptr) {
            *why = "the camera has no " + std::to_string(stream.width) + "x" + std::to_string(stream.height) + " mode";
            return false;
        }
        switch (stream.format) {
            case kFormatBlob:
                ++stills;
                break;
            case kFormatImplementationDefined:
            case kFormatYcbcr420:
                ++processed;
                break;
            default:
                *why = "the camera has no pixel format " + std::to_string(stream.format);
                return false;
        }
    }
    if (processed > kMaxProcessedStreams || stills > kMaxStillStreams) {
        *why = "too many streams";
        return false;
    }
    return true;
}

StreamSize hostSizeFor(const HostCamera& camera, const std::vector<StreamAsk>& streams) {
    const std::vector<StreamSize> sizes = streamSizes(camera);
    const std::vector<StreamSize> modes = modeSizes(camera);
    // The largest picture stream, or the still when there is none.
    const StreamAsk* target = nullptr;
    const StreamAsk* still = nullptr;
    for (const StreamAsk& stream : streams) {
        if (find(sizes, stream.width, stream.height) == nullptr) {
            continue;
        }
        const StreamAsk** slot = stream.format == kFormatBlob ? &still : &target;
        if (*slot == nullptr || area(stream.width, stream.height) > area((*slot)->width, (*slot)->height)) {
            *slot = &stream;
        }
    }
    if (target == nullptr) {
        target = still;
    }
    if (target == nullptr || modes.empty()) {
        return largest(camera);
    }
    // The smallest mode the stream fits in, one of its shape first, so the picture loses the
    // least to cropping and scaling.
    const StreamSize* best = nullptr;
    for (const StreamSize& mode : modes) {
        if (mode.width < target->width || mode.height < target->height) {
            continue;
        }
        if (best == nullptr) {
            best = &mode;
            continue;
        }
        const bool shape = sameShape(mode.width, mode.height, target->width, target->height);
        const bool bestShape = sameShape(best->width, best->height, target->width, target->height);
        if (shape != bestShape) {
            if (shape) {
                best = &mode;
            }
            continue;
        }
        if (area(mode.width, mode.height) < area(best->width, best->height)) {
            best = &mode;
        }
    }
    return best != nullptr ? *best : largest(camera);
}

std::string CameraIds::idFor(const std::string& hostId) {
    auto known = mByHost.find(hostId);
    if (known != mByHost.end()) {
        return known->second;
    }
    const std::string id = std::to_string(mNext++);
    mByHost.emplace(hostId, id);
    return id;
}

std::string CameraIds::hostIdOf(const std::string& id) const {
    for (const auto& [host, android] : mByHost) {
        if (android == id) {
            return host;
        }
    }
    return std::string();
}

}  // namespace dsa::camera
