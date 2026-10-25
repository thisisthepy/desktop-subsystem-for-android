// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// A gfxstream IOStream over a vsock connection to the host renderer. Unlike the stream the
// app drivers use, it remembers that the connection broke, so the hwcomposer can drop it
// and connect again instead of encoding into a dead socket.

#pragma once

#include <cstddef>
#include <cstdint>

#include "gfxstream/guest/IOStream.h"

namespace dsa::hwc {

class HostStream final : public gfxstream::guest::IOStream {
   public:
    HostStream(size_t bufferSize, uint32_t port);
    ~HostStream() override;

    // Connects to the host. Returns false, with the reason logged, if it cannot.
    bool open();
    // Whether a read or a write has failed since open().
    bool failed() const { return mFailed; }

    void* allocBuffer(size_t minSize) override;
    int commitBuffer(size_t size) override;
    const unsigned char* readFully(void* buf, size_t len) override;
    const unsigned char* commitBufferAndReadFully(size_t size, void* buf, size_t len) override;
    const unsigned char* read(void* buf, size_t* inoutLen) override;
    int writeFully(const void* buf, size_t len) override;

   private:
    int mFd = -1;
    uint32_t mPort;
    size_t mBufferSize;
    unsigned char* mBuffer = nullptr;
    bool mFailed = false;
};

}  // namespace dsa::hwc
