// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "host_renderer.h"

#include <cstring>
#include <string>

#include "ExtendedRenderControl.h"
#include "dsa_compose_guard.h"
#include "dsa_graphics_memory.h"
#include "dsa_graphics_memory_device.h"
#include "dsa_shared_color_buffer.h"
#include "gfxstream/guest/ChecksumCalculator.h"
#include "host_stream.h"
#include "cpu_upload_policy.h"
#include "log.h"

namespace dsa::hwc {

namespace {

// The gfxstream ports DSA's host listens on: the command stream and the process pipe.
constexpr uint32_t kStreamPort = 5566;
constexpr uint32_t kProcessPipePort = 5567;
constexpr size_t kStreamBufferSize = 1 << 20;
// The process pipe's handshake: the guest writes this, the host answers with the process's ID.
constexpr int32_t kProcessPipeConfirm = 100;
constexpr uint32_t kGlRgba = 0x1908;

}  // namespace

struct HostRenderer::Connection {
    std::unique_ptr<HostStream> processPipe;
    std::unique_ptr<HostStream> stream;
    gfxstream::guest::ChecksumCalculator checksum;
    std::unique_ptr<ExtendedRCEncoderContext> encoder;
    // The staging grant of the graphics memory the host shares, mapped, and whether the host
    // offers one (-1 until asked).
    dsa::SharedStaging staging;
    int sharesMemory = -1;

    bool ok() const { return !processPipe->failed() && !stream->failed(); }

    ~Connection() {
        // Let go of in the guest; the host revokes it when the process pipe closes.
        if (staging.ptr) {
            if (auto* shared = dsa::GraphicsMemoryDevice::get()) {
                shared->unmap(staging.ptr, staging.offset, staging.length);
            }
        }
    }
};

void HostRenderer::ConnectionDeleter::operator()(Connection* connection) const { delete connection; }

HostRenderer::HostRenderer() = default;

HostRenderer::~HostRenderer() = default;

HostRenderer::ConnectionPtr HostRenderer::open() {
    ConnectionPtr connection(new Connection());

    connection->processPipe = std::make_unique<HostStream>(4096, kProcessPipePort);
    if (!connection->processPipe->open()) {
        return nullptr;
    }
    uint64_t puid = 0;
    if (connection->processPipe->writeFully(&kProcessPipeConfirm, sizeof(kProcessPipeConfirm)) != 0 ||
        !connection->processPipe->readFully(&puid, sizeof(puid))) {
        HWC_LOGE("the host renderer did not answer the process pipe handshake");
        return nullptr;
    }

    connection->stream = std::make_unique<HostStream>(kStreamBufferSize, kStreamPort);
    if (!connection->stream->open()) {
        return nullptr;
    }
    // The gfxstream protocol begins with the client's flags; DSA's hwcomposer sets none.
    const uint32_t clientFlags = 0;
    if (connection->stream->writeFully(&clientFlags, sizeof(clientFlags)) != 0) {
        return nullptr;
    }

    connection->encoder = std::make_unique<ExtendedRCEncoderContext>(connection->stream.get(),
                                                                     &connection->checksum);
    ExtendedRCEncoderContext* rc = connection->encoder.get();
    rc->queryAndSetHostCompositionImpl();
    rc->rcSetPuid(rc, puid);
    connection->stream->flush();
    if (!connection->ok()) {
        return nullptr;
    }
    if (!rc->hasHostCompositionV2()) {
        // Without it rcCompose cannot say which display a frame is for.
        HWC_LOGE("the host renderer does not offer host composition v2; no task frames can be sent");
    }
    HWC_LOGI("connected to the host renderer as guest process %llu",
             static_cast<unsigned long long>(puid));
    return connection;
}

void HostRenderer::adopt(ConnectionPtr connection) {
    mConnection = std::move(connection);
    ++mGeneration;
    mHostYuv = -1;
    mHostGuardsTargets = -1;
    // Until the new host says it composes YUV layers, SurfaceFlinger draws them.
    setHostComposesYuv(false);
}

bool HostRenderer::hostHasYuv() {
    if (mHostYuv < 0 && connected()) {
        ExtendedRCEncoderContext* rc = mConnection->encoder.get();
        mHostYuv = rc->queryHostExtensions().find(DSA_YUV_COLOR_BUFFERS_EXTENSION) !=
                           std::string::npos
                       ? 1
                       : 0;
        HWC_LOGI("the host renderer %s YUV ColorBuffers", mHostYuv ? "makes" : "does not make");
        const bool composes =
            mHostYuv == 1 && rc->queryHostExtensions().find(DSA_YUV_COMPOSITION_EXTENSION) !=
                                 std::string::npos;
        setHostComposesYuv(composes);
        HWC_LOGI("the host renderer %s video layers itself", composes ? "composes" : "does not compose");
    }
    return mHostYuv == 1;
}

bool HostRenderer::hostGuardsComposeTargets() {
    if (mHostGuardsTargets < 0 && connected()) {
        ExtendedRCEncoderContext* rc = mConnection->encoder.get();
        mHostGuardsTargets =
            rc->queryHostExtensions().find(DSA_COMPOSE_GUARD_EXTENSION) != std::string::npos ? 1
                                                                                             : 0;
        HWC_LOGI("the host renderer %s; each task's frames are composed into %s",
                 mHostGuardsTargets ? "waits for a window's copy before composing into its buffer"
                                    : "does not wait for a window's copy before composing",
                 mHostGuardsTargets ? "one ColorBuffer while its window keeps up"
                                    : "two ColorBuffers in turn");
    }
    return mHostGuardsTargets == 1;
}

bool HostRenderer::uploadColorBuffer(uint32_t colorBuffer, const dsa::UploadLayout& layout,
                                     const uint8_t* packed) {
    if (!connected() || colorBuffer == 0 || !layout.valid()) {
        return false;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    const dsa::SharedStaging& staging = mConnection->staging;
    // The host answers once the ColorBuffer holds the planes.
    int result = 0;
    if (packed && packed == staging.ptr && staging.holds(layout.packedSize())) {
        // Packed into the staging grant: the host reads them where they are.
        rc->bindDmaDirectly(staging.ptr, staging.dmaAddress());
        result = rc->rcUpdateColorBufferDMA(rc, colorBuffer, 0, 0, layout.callWidth,
                                            layout.callHeight, layout.glFormat, layout.glType,
                                            staging.ptr,
                                            static_cast<uint32_t>(layout.packedSize()));
        rc->bindDmaDirectly(nullptr, 0);
    } else {
        result = rc->rcUpdateColorBuffer(rc, colorBuffer, 0, 0, layout.callWidth,
                                         layout.callHeight, layout.glFormat, layout.glType,
                                         const_cast<uint8_t*>(packed));
    }
    return result == 0 && connected();
}

uint8_t* HostRenderer::stagingFor(size_t bytes) {
    if (!connected() || bytes == 0) {
        return nullptr;
    }
    Connection& connection = *mConnection;
    if (connection.staging.holds(bytes)) {
        return connection.staging.ptr;
    }
    if (connection.sharesMemory < 0) {
        const bool offered = connection.encoder->queryHostExtensions().find(
                                 dsa::kGraphicsMemoryExtension) != std::string::npos;
        const char* why = nullptr;
        const bool mapped = dsa::GraphicsMemoryDevice::get(&why) != nullptr;
        connection.sharesMemory = offered && mapped ? 1 : 0;
        if (connection.sharesMemory) {
            HWC_LOGI("CPU uploads go through the graphics memory the host shares");
        } else {
            HWC_LOGI("CPU uploads go through the stream: %s",
                     offered ? why : "the host renderer shares no graphics memory");
        }
    }
    if (!connection.sharesMemory) {
        return nullptr;
    }
    auto* shared = dsa::GraphicsMemoryDevice::get();
    // A smaller grant goes first; the next one is the next MiB up, so pictures of one size
    // ask once.
    if (connection.staging.ptr) {
        shared->unmap(connection.staging.ptr, connection.staging.offset,
                      connection.staging.length);
        connection.staging = dsa::SharedStaging{};
    }
    const uint64_t size = (static_cast<uint64_t>(bytes) + (1u << 20) - 1) & ~uint64_t((1u << 20) - 1);
    const dsa::StagingRequest request{dsa::kStagingRequest, 0, size};
    dsa::StagingReply reply{};
    if (connection.processPipe->writeFully(&request, sizeof(request)) != 0 ||
        !connection.processPipe->readFully(&reply, sizeof(reply)) || reply.result != 0) {
        return nullptr;
    }
    uint8_t* mapped = shared->map(reply.offset, reply.length, reply.key);
    if (!mapped) {
        // Let the host have it back: a request of 0 bytes.
        const dsa::StagingRequest none{dsa::kStagingRequest, 0, 0};
        if (connection.processPipe->writeFully(&none, sizeof(none)) == 0) {
            connection.processPipe->readFully(&reply, sizeof(reply));
        }
        return nullptr;
    }
    connection.staging = dsa::SharedStaging{mapped, reply.offset, reply.length};
    return mapped;
}

bool HostRenderer::connected() const { return mConnection && mConnection->ok(); }

uint32_t HostRenderer::openSharedColorBuffer(const BufferRef& buffer) {
    if (!connected() || !buffer.valid()) {
        return 0;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    const uint32_t handle = dsa_shared_color_buffer_handle(buffer.id);
    if (rc->rcOpenColorBuffer2(rc, handle) == 0 && connected()) {
        return handle;
    }
    // No process has used the buffer yet. It gets the buffer's own format, the one every
    // other process (the app's Vulkan driver among them) would give it.
    const uint32_t width = buffer.width ? buffer.width : 1;
    const uint32_t height = buffer.height ? buffer.height : 1;
    rc->rcCreateColorBufferWithHandle(
        rc, width, height,
        dsa_shared_color_buffer_create_format(buffer.format, width, height, hostHasYuv() ? 1 : 0),
        handle);
    if (rc->rcOpenColorBuffer2(rc, handle) != 0 || !connected()) {
        HWC_LOGE("the host has no ColorBuffer %#x for gralloc buffer %u", handle, buffer.id);
        return 0;
    }
    return handle;
}

uint32_t HostRenderer::createColorBuffer(uint32_t width, uint32_t height) {
    if (!connected()) {
        return 0;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    const uint32_t handle = rc->rcCreateColorBuffer(rc, width, height, kGlRgba);
    if (handle == 0 || !connected()) {
        return 0;
    }
    // Opened, so the host counts it as this process's and frees it if the process dies.
    if (rc->rcOpenColorBuffer2(rc, handle) != 0) {
        return 0;
    }
    return handle;
}

void HostRenderer::closeColorBuffer(uint32_t handle) {
    if (connected() && handle != 0) {
        mConnection->encoder->rcCloseColorBuffer(mConnection->encoder.get(), handle);
    }
}

bool HostRenderer::createDisplay(uint32_t displayId, uint32_t width, uint32_t height) {
    if (!connected()) {
        return false;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    if (rc->rcCreateDisplayById(rc, displayId) != 0 || !connected()) {
        HWC_LOGE("the host renderer would not create display %u", displayId);
        return false;
    }
    return resizeDisplay(displayId, width, height);
}

bool HostRenderer::resizeDisplay(uint32_t displayId, uint32_t width, uint32_t height) {
    if (!connected()) {
        return false;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    // Where the window goes on the host is the host's to choose; only its size is the guest's.
    if (rc->rcSetDisplayPose(rc, displayId, -1, -1, width, height) != 0 || !connected()) {
        HWC_LOGE("the host renderer would not size display %u to %ux%u", displayId, width, height);
        return false;
    }
    return true;
}

void HostRenderer::destroyDisplay(uint32_t displayId) {
    if (connected()) {
        mConnection->encoder->rcDestroyDisplay(mConnection->encoder.get(), displayId);
    }
}

ComposeResult HostRenderer::compose(const std::vector<uint8_t>& request) {
    if (!connected()) {
        return ComposeResult::Failed;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    // The host answers whether it composed: 1 when it did, 0 or -1 when it did not, and
    // DSA_COMPOSE_TARGET_BUSY when the target's last frame was still being copied.
    const int result = rc->rcComposeWithoutPost(rc, static_cast<uint32_t>(request.size()),
                                               const_cast<uint8_t*>(request.data()));
    if (!connected()) {
        return ComposeResult::Failed;
    }
    if (result == DSA_COMPOSE_TARGET_BUSY) {
        return ComposeResult::TargetBusy;
    }
    return result > 0 ? ComposeResult::Composed : ComposeResult::Failed;
}

void HostRenderer::post(uint32_t displayId, uint32_t colorBuffer) {
    if (!connected()) {
        return;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    // The host copies the ColorBuffer into the display's window when it becomes the
    // display's current one, so this is the frame's delivery.
    rc->rcSetDisplayColorBuffer(rc, displayId, colorBuffer);
}

}  // namespace dsa::hwc
