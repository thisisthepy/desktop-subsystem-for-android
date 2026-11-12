// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Copies what a CPU wrote into a layer's buffer up to the buffer's host ColorBuffer, before the
// host composes the layer or SurfaceFlinger draws it (dsa_cpu_upload.h has why and the rules).
// The hwcomposer is handed every layer SurfaceFlinger shows, client layers included, so it is
// where those buffers are copied: once for each new picture, and again only when the picture
// in a buffer it has seen changes.
//
// The planes are read through a gralloc CPU lock, the mapping every writer uses, from a clone
// of the buffer's handle. libnativewindow is opened when first needed: the binary links only
// the platform's C libraries, and the NDK's stub of it has no AHardwareBuffer_createFromHandle.

#pragma once

#include <aidl/android/hardware/common/NativeHandle.h>

#include <cstdint>
#include <memory>
#include <vector>

#include "cpu_upload_policy.h"
#include "dsa_cpu_upload.h"
#include "host_renderer.h"

namespace dsa::hwc {

// A gralloc handle whose descriptors this process owns: SurfaceFlinger names a buffer by its
// slot after the first frame, so its descriptors are kept for the later uploads.
struct GrallocHandle {
    std::vector<int> fds;
    std::vector<int> ints;

    GrallocHandle() = default;
    GrallocHandle(const GrallocHandle&) = delete;
    GrallocHandle& operator=(const GrallocHandle&) = delete;
    ~GrallocHandle();

    // Duplicates the handle's descriptors. Null when one cannot be duplicated.
    static std::shared_ptr<GrallocHandle> from(
        const aidl::android::hardware::common::NativeHandle& handle);
};

class CpuUploads {
   public:
    // Copies the buffer up to `colorBuffer` if `fresh` (the layer's buffer changed, so it is a
    // new picture) or its fingerprint changed. Returns false when the buffer could not be read
    // or the host refused it; true when it was copied or did not need to be.
    bool upload(HostRenderer& host, uint32_t colorBuffer, const BufferRef& buffer, bool fresh);
    void forget(uint32_t bufferId) { mTracker.forget(bufferId); }
    // A new host connection: its ColorBuffers hold nothing yet.
    void clear() { mTracker = dsa::UploadTracker{}; }

   private:
    dsa::UploadTracker mTracker;
    std::vector<uint8_t> mStaging;
    dsa::UploadRate mRate;
    // A failure that repeats on every picture is logged at its first and then once in a
    // while, with its count: a failure logged only once in a boot hid that no upload ever
    // succeeded.
    UploadFailureLog mFailures;
};

}  // namespace dsa::hwc
