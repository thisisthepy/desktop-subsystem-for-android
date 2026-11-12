// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "cpu_upload.h"

#include <android/hardware_buffer.h>
#include <cutils/native_handle.h>
#include <dlfcn.h>
#include <unistd.h>

#include <cinttypes>
#include <cstring>
#include <ctime>
#include <mutex>

#include "dsa_hardware_buffer_import.h"
#include "log.h"

namespace dsa::hwc {

namespace {

// The libnativewindow calls a CPU lock needs, opened once.
struct NativeWindow {
    using CreateFromHandle = int (*)(const AHardwareBuffer_Desc*, const native_handle_t*, int32_t,
                                     AHardwareBuffer**);
    using Lock = int (*)(AHardwareBuffer*, uint64_t, int32_t, const ARect*, void**);
    using Unlock = int (*)(AHardwareBuffer*, int32_t*);
    using Release = void (*)(AHardwareBuffer*);

    CreateFromHandle createFromHandle = nullptr;
    Lock lock = nullptr;
    Unlock unlock = nullptr;
    Release release = nullptr;

    bool ok() const { return createFromHandle && lock && unlock && release; }

    static const NativeWindow& get() {
        static NativeWindow calls;
        static std::once_flag once;
        std::call_once(once, [] {
            void* library = dlopen("libnativewindow.so", RTLD_NOW | RTLD_LOCAL);
            if (!library) {
                HWC_LOGE("cannot open libnativewindow.so (%s): what a CPU writes into a layer "
                         "will not reach the host",
                         dlerror());
                return;
            }
            calls.createFromHandle = reinterpret_cast<CreateFromHandle>(
                dlsym(library, "AHardwareBuffer_createFromHandle"));
            calls.lock = reinterpret_cast<Lock>(dlsym(library, "AHardwareBuffer_lock"));
            calls.unlock = reinterpret_cast<Unlock>(dlsym(library, "AHardwareBuffer_unlock"));
            calls.release = reinterpret_cast<Release>(dlsym(library, "AHardwareBuffer_release"));
            if (!calls.ok()) {
                HWC_LOGE("libnativewindow.so lacks a CPU lock call: what a CPU writes into a "
                         "layer will not reach the host");
            }
        });
        return calls;
    }
};

uint64_t monotonicNs() {
    timespec now = {};
    clock_gettime(CLOCK_MONOTONIC, &now);
    return uint64_t(now.tv_sec) * 1000000000ULL + uint64_t(now.tv_nsec);
}

}  // namespace

GrallocHandle::~GrallocHandle() {
    for (int fd : fds) {
        if (fd >= 0) {
            close(fd);
        }
    }
}

std::shared_ptr<GrallocHandle> GrallocHandle::from(
    const aidl::android::hardware::common::NativeHandle& handle) {
    auto owned = std::make_shared<GrallocHandle>();
    owned->fds.reserve(handle.fds.size());
    for (const auto& fd : handle.fds) {
        const int copy = fd.get() >= 0 ? dup(fd.get()) : -1;
        if (fd.get() >= 0 && copy < 0) {
            return nullptr;
        }
        owned->fds.push_back(copy);
    }
    owned->ints = handle.ints;
    return owned;
}

bool CpuUploads::upload(HostRenderer& host, uint32_t colorBuffer, const BufferRef& buffer,
                        bool fresh) {
    if (!buffer.cpuWritten() || colorBuffer == 0) {
        return true;
    }
    const dsa::UploadLayout layout =
        dsa::uploadLayout(buffer.format, buffer.width, buffer.height, host.hostHasYuv());
    if (!layout.valid()) {
        // Its ColorBuffer cannot hold its planes as they are (a YUV buffer on a host without
        // YUV ColorBuffers, or a format with no upload).
        return true;
    }
    const NativeWindow& calls = NativeWindow::get();
    if (!calls.ok()) {
        return false;
    }

    // The handle as gralloc laid it out: the descriptors, then the ints.
    const GrallocHandle& owned = *buffer.handle;
    std::vector<uint8_t> storage(sizeof(native_handle_t) +
                                 (owned.fds.size() + owned.ints.size()) * sizeof(int));
    auto* native = reinterpret_cast<native_handle_t*>(storage.data());
    native->version = sizeof(native_handle_t);
    native->numFds = static_cast<int>(owned.fds.size());
    native->numInts = static_cast<int>(owned.ints.size());
    std::memcpy(&native->data[0], owned.fds.data(), owned.fds.size() * sizeof(int));
    std::memcpy(&native->data[owned.fds.size()], owned.ints.data(), owned.ints.size() * sizeof(int));

    // The buffer as it was allocated: the mapper checks a description against the handle.
    const dsa::HardwareBufferImport import = dsa::cloneImportFor(dsa::GrallocBufferFacts{
        buffer.width, buffer.height, buffer.androidFormat, buffer.usage, buffer.pixelStride});
    AHardwareBuffer_Desc desc = {};
    desc.width = import.width;
    desc.height = import.height;
    desc.layers = import.layers;
    desc.format = import.format;
    desc.usage = import.usage;
    desc.stride = import.stride;
    AHardwareBuffer* ahb = nullptr;
    int result = calls.createFromHandle(&desc, native, import.method, &ahb);
    if (result != 0 || !ahb) {
        uint32_t failures = 0;
        if (mFailures.shouldLog(&failures)) {
            HWC_LOGE("cannot import gralloc buffer %u (%ux%u, format %#x) to copy what the CPU "
                     "wrote: %d (%u in a row); its layer shows what the host held before",
                     buffer.id, buffer.width, buffer.height, buffer.format, result, failures);
        }
        return false;
    }

    // The lock maps the first plane at its offset; the others follow at theirs.
    const uint32_t planes = buffer.planeCount < 4 ? buffer.planeCount : 4;
    const uint64_t first = planes ? buffer.offsets[0] : 0;
    dsa::SourcePlane sources[4];
    for (uint32_t i = 0; i < planes; ++i) {
        sources[i] = dsa::SourcePlane{
            buffer.offsets[i] >= first ? buffer.offsets[i] - first : UINT64_MAX / 2,
            buffer.strides[i]};
    }
    const size_t mapped = buffer.planeBytes > first ? size_t(buffer.planeBytes - first) : 0;

    void* address = nullptr;
    result = calls.lock(ahb, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN, -1, nullptr, &address);
    bool send = false;
    bool ok = result == 0 && address;
    uint64_t print = 0;
    if (ok) {
        const uint8_t* bytes = static_cast<const uint8_t*>(address);
        print = dsa::fingerprint(bytes, mapped, sources, planes, layout);
        ok = print != 0;
        if (ok && mTracker.needsUpload(buffer.id, print, fresh)) {
            mStaging.resize(layout.packedSize());
            send = ok = dsa::packPlanes(bytes, mapped, sources, planes, layout, mStaging.data());
        }
        calls.unlock(ahb, nullptr);
    }
    calls.release(ahb);
    if (!ok) {
        uint32_t failures = 0;
        if (mFailures.shouldLog(&failures)) {
            HWC_LOGE("cannot read gralloc buffer %u (%ux%u, format %#x) to copy what the CPU "
                     "wrote: lock %d (%u in a row)",
                     buffer.id, buffer.width, buffer.height, buffer.format, result, failures);
        }
        return false;
    }
    if (!send) {
        return true;
    }
    if (!host.uploadColorBuffer(colorBuffer, layout, mStaging.data())) {
        uint32_t failures = 0;
        if (mFailures.shouldLog(&failures)) {
            HWC_LOGE("the host did not take what the CPU wrote into gralloc buffer %u (%ux%u, "
                     "format %#x) (%u in a row)",
                     buffer.id, buffer.width, buffer.height, buffer.format, failures);
        }
        return false;
    }
    mFailures.reset();
    mTracker.uploaded(buffer.id, print);
    dsa::UploadRate::Report report;
    if (mRate.add(monotonicNs(), mStaging.size(), &report)) {
        HWC_LOGD("copied %" PRIu64 " KiB that a CPU wrote up to the host in %u uploads over "
                 "%" PRIu64 " ms",
                 report.bytes / 1024, report.uploads, report.windowNs / 1000000);
    }
    return true;
}

}  // namespace dsa::hwc
