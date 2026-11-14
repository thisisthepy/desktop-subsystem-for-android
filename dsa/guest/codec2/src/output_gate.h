// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Whether a decoded picture may still go out, and the waits a picture makes for its buffer.
//
// A picture waits twice on its way to the app's surface: for a buffer of the surface to come
// back (the allocator's waitable descriptor), then for the surface's consumer to stop reading
// that buffer (its release fence). A flush, a stop or a reset makes every picture of the
// stream before it worthless, so both waits end as soon as one happens. Without that, a flush
// waited for each picture still queued ahead of the host's answer, up to 2 s each, and the app
// waited with it.

#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>

namespace dsa::codec2 {

class OutputGate {
   public:
    // The ticket of a picture that arrives now.
    uint64_t ticket() const { return mEpoch.load(); }
    // Whether a picture that arrived with `ticket` may still go out: no flush, stop or reset
    // has happened since, and no flush is going on.
    bool open(uint64_t ticket) const { return !mFlushing.load() && mEpoch.load() == ticket; }
    // A flush starts: every picture that arrived before it, and every one that arrives until
    // the host has answered it, belongs to the stream before the flush.
    void beginFlush() {
        mFlushing.store(true);
        mEpoch.fetch_add(1);
    }
    void endFlush() { mFlushing.store(false); }
    // A stop or a reset: every picture so far is of a stream that has ended.
    void stop() { mEpoch.fetch_add(1); }

   private:
    std::atomic<uint64_t> mEpoch{0};
    std::atomic<bool> mFlushing{false};
};

enum class WaitResult {
    // The descriptor became readable: the fence signalled, or a buffer came back.
    Ready,
    // The deadline passed first.
    TimedOut,
    // The gate closed for the ticket: a flush, a stop or a reset.
    Closed,
    // The descriptor is in error, or polling it failed.
    Failed,
};

// Waits until `fd` is readable, `deadline` passes or `gate` closes for `ticket`, looking at the
// gate every `slice`. A negative `fd` has nothing to poll: the wait lasts until the deadline or
// the gate.
WaitResult waitReadable(int fd, std::chrono::steady_clock::time_point deadline, const OutputGate& gate,
                        uint64_t ticket, std::chrono::milliseconds slice = std::chrono::milliseconds(50));

}  // namespace dsa::codec2
