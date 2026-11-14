// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// One decoder's connection to the host's video port (vsock 5569). The host decodes the
// stream on its own GPU. Packets are queued by the component's thread and written by a
// thread of the link's own (send_queue.h says why), and another reads the pictures back and
// hands each to the component (message_reader.h).

#pragma once

#include <atomic>
#include <chrono>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "message_reader.h"
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
    using MessageHandler = MessageReader::Handler;

    ~HostDecoder();

    // Opens a decoder on the host. On failure returns nothing and says why in `error`.
    static std::unique_ptr<HostDecoder> open(Codec codec, uint32_t width, uint32_t height,
                                             MessageHandler onMessage, std::string* error);

    // A link over `fd`, a socket already past the open: the reader and the writer start at
    // once. What open() makes, and what the tests make over a socket pair.
    static std::unique_ptr<HostDecoder> adopt(int fd, MessageHandler onMessage);

    // Queues one encoded Envelope for the host, in order, without waiting for the host to
    // read it, unless what waits unsent is at its bound: then it waits at most `wait`. A
    // `droppable` frame is one a flush makes worthless (a packet of a picture). Closed once
    // the host has gone.
    PushResult send(const std::vector<uint8_t>& frame, bool droppable = false,
                    std::chrono::milliseconds wait = std::chrono::milliseconds(500));

    // Queues a flush in the place of the packets still unsent, at once: the host would only
    // decode them before it read the flush. False once the host has gone.
    bool sendFlush(const std::vector<uint8_t>& frame);

    // Closes the connection and ends the reader and the writer; no message is handed on after
    // (see MessageReader::stop() for a close from inside the handler).
    void close();

   private:
    HostDecoder(int fd, MessageHandler onMessage);
    void write();

    int mFd;
    SendQueue mOutgoing;
    std::atomic<bool> mClosed{false};
    std::atomic<bool> mWriteFailed{false};
    std::unique_ptr<MessageReader> mReader;
    std::thread mWriter;
};

}  // namespace dsa::codec2
