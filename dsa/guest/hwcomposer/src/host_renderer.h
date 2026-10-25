// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The hwcomposer's connection to the host renderer: renderControl over the gfxstream
// stream (vsock port 5566), with the process pipe (port 5567) that tells the host which
// guest process owns what, so the host frees it all if the hwcomposer dies.
//
// Not thread safe: TaskComposer serialises every call. Connecting is the exception: open()
// touches nothing shared, so it can run while frames are being composed, and only adopt()
// needs the caller's lock.

#pragma once

#include <cstdint>
#include <memory>
#include <vector>

namespace dsa::hwc {

// A gralloc buffer as the hwcomposer sees it: what its handle says about it.
struct BufferRef {
    // The allocator's buffer ID; 0 when the handle is not a minigbm buffer.
    uint32_t id = 0;
    uint32_t width = 0;
    uint32_t height = 0;

    bool valid() const { return id != 0; }
    bool operator==(const BufferRef& o) const {
        return id == o.id && width == o.width && height == o.height;
    }
};

class HostRenderer {
   public:
    struct Connection;
    // Defined where Connection is, so callers need not see it.
    struct ConnectionDeleter {
        void operator()(Connection* connection) const;
    };
    using ConnectionPtr = std::unique_ptr<Connection, ConnectionDeleter>;

    HostRenderer();
    ~HostRenderer();

    // Connects to the host renderer. Blocks while it does, and returns null, with the reason
    // logged, when the host is not there.
    static ConnectionPtr open();
    // Takes a new connection in place of the old one, if any.
    void adopt(ConnectionPtr connection);

    // Whether there is a connection and nothing on it has failed.
    bool connected() const;
    // Counts connections. Host objects (displays, ColorBuffers) made on an earlier one are
    // gone with it, so a caller compares this against the value it made them under.
    uint64_t generation() const { return mGeneration; }

    // Opens the host ColorBuffer every guest process shares for this gralloc buffer,
    // creating it if no process has yet. Returns 0 if the host has none to give.
    uint32_t openSharedColorBuffer(const BufferRef& buffer);
    // A ColorBuffer of its own, opened for this process. 0 on failure.
    uint32_t createColorBuffer(uint32_t width, uint32_t height);
    void closeColorBuffer(uint32_t handle);

    bool createDisplay(uint32_t displayId, uint32_t width, uint32_t height);
    bool resizeDisplay(uint32_t displayId, uint32_t width, uint32_t height);
    void destroyDisplay(uint32_t displayId);

    // Composes a ComposeDevice_v2 request and waits until the host has finished.
    bool compose(const std::vector<uint8_t>& request);
    // Makes `colorBuffer`, already composed, display `displayId`'s current frame: the host
    // shows it in the task's window then.
    void post(uint32_t displayId, uint32_t colorBuffer);

   private:
    ConnectionPtr mConnection;
    uint64_t mGeneration = 0;
};

}  // namespace dsa::hwc
