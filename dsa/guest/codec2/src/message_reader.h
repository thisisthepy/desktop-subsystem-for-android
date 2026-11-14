// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The thread that reads the host's messages off a decoder's socket and hands each on.
//
// The handler it hands them to may end the reader's own owner. The component goes away when
// its last reference does, and once the app has died that reference can be the one the
// handler holds while it takes a message: the component is then destroyed on the reading
// thread, from inside the handler, and closes its link there. The reader used to keep its
// handler and its flags in the link itself, so after the handler returned it read them from
// freed memory, and called an empty std::function: the HAL aborted with bad_function_call.
// Everything the thread uses now lives in a state of its own that the thread shares, so it
// outlives the link, and a reader stopped from its own thread ends as soon as the handler
// returns.

#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#include "video_messages.h"

namespace dsa::codec2 {

// The largest frame the host may send: a 4096 by 2304 picture with its framing, and room.
constexpr uint32_t kMaxHostFrame = 16 * 1024 * 1024;

// Reads one frame (its length in front) from `fd`. False on an error, the end of the stream,
// or a length past kMaxHostFrame.
bool readHostFrame(int fd, std::vector<uint8_t>* frame);

class MessageReader {
   public:
    using Handler = std::function<void(HostMessage)>;

    // Starts reading `fd`, which stays its owner's to shut down and close. Each message goes
    // to `handler`; when the socket ends before stop(), the handler gets a DecodeError that
    // says the host's decoder went away.
    MessageReader(int fd, Handler handler);
    // Stops the reader (see stop()).
    ~MessageReader();

    MessageReader(const MessageReader&) = delete;
    MessageReader& operator=(const MessageReader&) = delete;

    // No message is handed on after this returns, except one the handler is already taking;
    // it does not wait for that. The owner calls it before it shuts the socket down, so the
    // end of the socket is not reported as the host going away.
    void silence();

    // No message is handed on after this returns, except from the reading thread itself,
    // which this does not wait for: called there (from the handler), the thread ends when the
    // handler returns. Elsewhere it waits for the thread, so the owner shuts the socket down
    // first to end a read in progress.
    void stop();

   private:
    struct State {
        int fd = -1;
        std::mutex lock;
        Handler handler;
        std::atomic<bool> stopped{false};
    };

    static void run(const std::shared_ptr<State>& state);

    std::shared_ptr<State> mState;
    std::thread mThread;
};

}  // namespace dsa::codec2
