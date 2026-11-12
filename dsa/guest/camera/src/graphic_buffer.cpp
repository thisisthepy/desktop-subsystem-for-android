// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "graphic_buffer.h"

#include <poll.h>
#include <unistd.h>

#include <cerrno>
#include <cstdlib>
#include <cstring>

#include "dsa_hardware_buffer_import.h"

namespace {

// The platform's native_handle_t: a version, the counts, then the fds and the ints.
struct NativeHandleHeader {
    int version;
    int numFds;
    int numInts;
};

}  // namespace

extern "C" int AHardwareBuffer_createFromHandle(const AHardwareBuffer_Desc* desc, const void* handle,
                                                int32_t method, AHardwareBuffer** outBuffer);

namespace dsa::camera {

AHardwareBuffer* importBuffer(const HandleParts& handle, uint32_t width, uint32_t height, uint32_t format,
                              uint64_t usage) {
    if (handle.fds.empty()) {
        return nullptr;
    }
    const size_t count = handle.fds.size() + handle.ints.size();
    const size_t bytes = sizeof(NativeHandleHeader) + count * sizeof(int);
    auto* header = static_cast<NativeHandleHeader*>(std::calloc(1, bytes));
    if (header == nullptr) {
        return nullptr;
    }
    header->version = static_cast<int>(sizeof(NativeHandleHeader));
    header->numFds = static_cast<int>(handle.fds.size());
    header->numInts = static_cast<int>(handle.ints.size());
    int* data = reinterpret_cast<int*>(header + 1);
    for (size_t i = 0; i < handle.fds.size(); ++i) {
        data[i] = handle.fds[i];
    }
    for (size_t i = 0; i < handle.ints.size(); ++i) {
        data[handle.fds.size() + i] = handle.ints[i];
    }
    AHardwareBuffer_Desc desc{};
    desc.width = width;
    desc.height = height;
    desc.layers = 1;
    desc.format = format;
    desc.usage = usage;
    desc.stride = width;
    AHardwareBuffer* buffer = nullptr;
    const int result = AHardwareBuffer_createFromHandle(&desc, header, dsa::kAhbCreateFromHandleClone, &buffer);
    std::free(header);
    return result == 0 ? buffer : nullptr;
}

bool fillPicture(AHardwareBuffer* buffer, const Nv12& picture, uint32_t width, uint32_t height, std::string* error) {
    AHardwareBuffer_Planes planes{};
    if (AHardwareBuffer_lockPlanes(buffer, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &planes) == 0) {
        bool written = false;
        if (planes.planeCount >= 3) {
            YuvPlanes yuv;
            yuv.y = Plane{static_cast<uint8_t*>(planes.planes[0].data), planes.planes[0].pixelStride,
                          planes.planes[0].rowStride};
            yuv.u = Plane{static_cast<uint8_t*>(planes.planes[1].data), planes.planes[1].pixelStride,
                          planes.planes[1].rowStride};
            yuv.v = Plane{static_cast<uint8_t*>(planes.planes[2].data), planes.planes[2].pixelStride,
                          planes.planes[2].rowStride};
            written = scaleToYuv(picture, width, height, yuv);
        } else if (planes.planeCount == 1) {
            written = scaleToRgba(picture, width, height, static_cast<uint8_t*>(planes.planes[0].data),
                                  planes.planes[0].rowStride);
        }
        AHardwareBuffer_unlock(buffer, nullptr);
        if (!written) {
            *error = "the picture did not fit the buffer";
        }
        return written;
    }
    // An RGBA buffer the allocator will not lock by planes.
    AHardwareBuffer_Desc desc{};
    AHardwareBuffer_describe(buffer, &desc);
    void* data = nullptr;
    if (AHardwareBuffer_lock(buffer, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &data) != 0) {
        *error = "the buffer cannot be written";
        return false;
    }
    const bool written = scaleToRgba(picture, width, height, static_cast<uint8_t*>(data), desc.stride * 4);
    AHardwareBuffer_unlock(buffer, nullptr);
    if (!written) {
        *error = "the picture did not fit the buffer";
    }
    return written;
}

bool fillStill(AHardwareBuffer* buffer, size_t size, const std::string& jpeg, std::string* error) {
    void* data = nullptr;
    if (AHardwareBuffer_lock(buffer, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &data) != 0) {
        *error = "the still buffer cannot be written";
        return false;
    }
    const bool written = writeJpegBlob(static_cast<uint8_t*>(data), size,
                                       reinterpret_cast<const uint8_t*>(jpeg.data()), jpeg.size());
    AHardwareBuffer_unlock(buffer, nullptr);
    if (!written) {
        *error = "the still is larger than its buffer";
    }
    return written;
}

bool waitFence(int fd, int timeoutMs) {
    if (fd < 0) {
        return true;
    }
    pollfd waiting{fd, POLLIN, 0};
    int result = 0;
    do {
        result = poll(&waiting, 1, timeoutMs);
    } while (result < 0 && errno == EINTR);
    close(fd);
    return result > 0;
}

}  // namespace dsa::camera
