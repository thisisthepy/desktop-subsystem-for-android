// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The host's pictures, in the order they came, waiting for the component's own thread to put
// each into a buffer of the app's surface.
//
// The thread that reads the host's socket used to write each picture itself. A picture waits
// for a buffer of the surface, sometimes for most of a frame; while it waited, nothing read
// the socket, the host could not send the next picture, and its decoder stood still. The
// stream then played at the pace of its slowest buffer. The reader now only puts each
// picture here and goes back to the socket, and the host decodes on.
//
// What waits is bounded by a count and by bytes, since each picture is a decoded frame in
// memory: a reader that finds the queue full waits, which holds the host back as before.

#pragma once

#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <mutex>

#include "video_messages.h"

namespace dsa::codec2 {

// One message from the host, as it came.
struct Arrival {
    HostMessage message;
    // The output gate's ticket when it came (output_gate.h): a flush after it makes it stale.
    uint64_t ticket = 0;
    std::chrono::steady_clock::time_point arrived;
};

class ArrivalQueue {
   public:
    ArrivalQueue(size_t maxCount, size_t maxBytes) : mMaxCount(maxCount), mMaxBytes(maxBytes) {}

    // Puts `arrival` behind the ones queued. Waits while `maxCount` are queued or its bytes
    // would pass `maxBytes`; one always fits an empty queue. False once the queue is closed.
    bool push(Arrival arrival);

    // Takes the oldest, waiting for one. False once the queue is closed.
    bool pop(Arrival* arrival);

    // Ends the queue: every waiting push and pop returns false, and what was queued is dropped.
    void close();

    // Opens a closed queue again, empty, for a decoder that starts again.
    void open();

    // How many wait.
    size_t size();

    // The bytes of picture an arrival holds.
    static size_t bytesOf(const Arrival& arrival);

   private:
    const size_t mMaxCount;
    const size_t mMaxBytes;
    std::mutex mLock;
    std::condition_variable mChanged;
    std::deque<Arrival> mQueued;
    size_t mBytes = 0;
    bool mClosed = false;
};

}  // namespace dsa::codec2
