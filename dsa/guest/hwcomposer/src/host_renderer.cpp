// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "host_renderer.h"

#include <cstring>
#include <string>

#include "ExtendedRenderControl.h"
#include "dsa_shared_color_buffer.h"
#include "gfxstream/guest/ChecksumCalculator.h"
#include "host_stream.h"
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

    bool ok() const { return !processPipe->failed() && !stream->failed(); }
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
    // No process has used the buffer yet.
    rc->rcCreateColorBufferWithHandle(rc, buffer.width ? buffer.width : 1,
                                      buffer.height ? buffer.height : 1, kGlRgba, handle);
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

bool HostRenderer::compose(const std::vector<uint8_t>& request) {
    if (!connected()) {
        return false;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    // The host answers whether it composed: 1 when it did, 0 or -1 when it did not.
    const int result = rc->rcComposeWithoutPost(rc, static_cast<uint32_t>(request.size()),
                                               const_cast<uint8_t*>(request.data()));
    return result > 0 && connected();
}

void HostRenderer::post(uint32_t displayId, uint32_t colorBuffer) {
    if (!connected()) {
        return;
    }
    ExtendedRCEncoderContext* rc = mConnection->encoder.get();
    rc->rcSetDisplayColorBuffer(rc, displayId, colorBuffer);
    rc->rcFBPost(rc, colorBuffer);
    mConnection->stream->flush();
}

}  // namespace dsa::hwc
