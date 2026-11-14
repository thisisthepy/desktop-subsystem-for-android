// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// One decoder's connection to the host's video port (vsock 5569). The host decodes the
// stream on its own GPU. Packets are queued by the component's thread and written by a
// thread of the link's own (send_queue.h says why), and another reads the pictures back and
// hands each to the component.

#pragma once

#include <atomic>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "send_queue.h"
#include "video_messages.h"

namespace dsa::codec2 {

// The vsock port of the host's video decoders.
constexpr uint32_t kVideoPort = 5569;

// The formats the host decodes on its GPU, asked on a connection of its own. Nothing when
// the host is not there (its window manager is not running) or refuses.
std::optional<std::vector<CodecSupport>> queryCapabilities();

class HostDecoder {
   public:
    // Takes each message as it comes, on the reading thread.
    using MessageHandler = std::function<void(HostMessage)>;

    ~HostDecoder();

    // Opens a decoder on the host. On failure returns nothing and says why in `error`.
    static std::unique_ptr<HostDecoder> open(Codec codec, uint32_t width, uint32_t height,
                                             MessageHandler onMessage, std::string* error);

    // Queues one encoded Envelope for the host, in order, without waiting for the host to
    // read it. False once the host has gone.
    bool send(const std::vector<uint8_t>& frame);

    // Closes the connection and waits for the reader and the writer to end; no message is
    // handed on after.
    void close();

   private:
    HostDecoder(int fd, MessageHandler onMessage);
    void read();
    void write();

    int mFd;
    MessageHandler mOnMessage;
    SendQueue mOutgoing;
    std::atomic<bool> mClosed{false};
    std::atomic<bool> mWriteFailed{false};
    std::thread mReader;
    std::thread mWriter;
};

}  // namespace dsa::codec2
