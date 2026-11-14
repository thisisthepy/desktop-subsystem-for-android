// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The frames a decoder sends to the host, queued for a thread of the link's own to write.
//
// The framework's queue() call used to write each packet to the socket itself. The host
// decodes on one thread: while it waits for the guest to read a picture, it reads no packet,
// and the socket fills. The guest reads the next picture only once it has a buffer of the
// app's surface, and the app gives buffers back from the thread that was waiting in queue().
// Each picture then waited its full 2 s for a buffer and was dropped, and queue() took 8 s.
// Now queue() only puts the packet here, and waits for room only as long as it is told to.
//
// A flush makes every packet still waiting here worthless: the host would decode each before
// it read the flush, and the framework's flush() call waited for all of them. A packet is
// queued as droppable, and a flush takes the place of the droppable packets still unsent.

#pragma once

#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <mutex>
#include <vector>

namespace dsa::codec2 {

enum class PushResult {
    Queued,
    // The queue is closed: the link has ended.
    Closed,
    // No room came in the time given: the host has stopped reading.
    Full,
};

class SendQueue {
   public:
    // `maxBytes` bounds what may wait unsent, so a host that never reads again cannot make the
    // queue grow without end.
    explicit SendQueue(size_t maxBytes) : mMaxBytes(maxBytes) {}

    // Puts `frame` behind the frames already queued and returns at once, unless `maxBytes` are
    // already waiting: then it waits for room. False once the queue is closed.
    bool push(std::vector<uint8_t> frame);

    // As push(), waiting at most `wait` for room. A `droppable` frame is one a flush makes
    // worthless (pushReplacingDroppable()).
    PushResult pushFor(std::vector<uint8_t> frame, bool droppable, std::chrono::milliseconds wait);

    // Drops every droppable frame still unsent, then queues `frame` behind what is left, at
    // once, whatever the bound. Returns how many were dropped, or -1 once the queue is closed.
    int pushReplacingDroppable(std::vector<uint8_t> frame);

    // Takes the oldest frame, waiting for one. False once the queue is closed.
    bool pop(std::vector<uint8_t>* frame);

    // Ends the queue: every waiting push and pop returns false, and what was queued is dropped.
    void close();

    // The bytes waiting unsent.
    size_t queuedBytes();

   private:
    struct Queued {
        std::vector<uint8_t> bytes;
        bool droppable = false;
    };

    bool hasRoom(size_t bytes) const { return mBytes == 0 || mBytes + bytes <= mMaxBytes; }
    void add(std::vector<uint8_t> frame, bool droppable);

    const size_t mMaxBytes;
    std::mutex mLock;
    std::condition_variable mChanged;
    std::deque<Queued> mFrames;
    size_t mBytes = 0;
    bool mClosed = false;
};

}  // namespace dsa::codec2
