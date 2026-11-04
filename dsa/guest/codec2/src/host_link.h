// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// One decoder's connection to the host's video port (vsock 5569). The host decodes the
// stream on its own GPU: packets go out on the component's thread, and a thread of the
// link's own reads the pictures back and hands each to the component.

#pragma once

#include <atomic>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "video_messages.h"

namespace dsa::codec2 {

// The vsock port of the host's video decoders.
constexpr uint32_t kVideoPort = 5569;

// The formats the host decodes on its GPU, asked on a connection of its own. Nothing when
// the host is not there (its window manager is not running) or refuses.
std::optional<std::vector<CodecSupport>> queryCapabilities();

class HostDecoder {
   public:
    using MessageHandler = std::function<void(const HostMessage&)>;

    ~HostDecoder();

    // Opens a decoder on the host. On failure returns nothing and says why in `error`.
    static std::unique_ptr<HostDecoder> open(Codec codec, uint32_t width, uint32_t height,
                                             MessageHandler onMessage, std::string* error);

    // Sends one encoded Envelope. False once the host has gone.
    bool send(const std::vector<uint8_t>& frame);

    // Closes the connection and waits for the reader to end; no message is handed on after.
    void close();

   private:
    HostDecoder(int fd, MessageHandler onMessage);
    void read();

    int mFd;
    MessageHandler mOnMessage;
    std::mutex mSendLock;
    std::atomic<bool> mClosed{false};
    std::thread mReader;
};

}  // namespace dsa::codec2
