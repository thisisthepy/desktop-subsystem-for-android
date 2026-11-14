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

// A flush no longer waits for the host's answer before it returns: the framework's flush()
// call returns within a short wait whatever the host does. Every flush the host has not
// answered yet keeps the gate closed, so a picture of the stream before a flush that comes
// after flush() returned is still dropped; the host sends the pictures of the packets after
// the flush only after its answer.
class OutputGate {
   public:
    // The ticket of a picture that arrives now.
    uint64_t ticket() const { return mEpoch.load(); }
    // Whether a picture that arrived with `ticket` may still go out: no flush, stop or reset
    // has happened since, and the host has answered every flush.
    bool open(uint64_t ticket) const { return mFlushes.load() == 0 && mEpoch.load() == ticket; }
    // A flush starts: every picture that arrived before it, and every one that arrives until
    // the host has answered it, belongs to the stream before the flush.
    void beginFlush() {
        mFlushes.fetch_add(1);
        mEpoch.fetch_add(1);
    }
    // The host answered one flush, or a flush was never sent (the host had gone).
    void endFlush() {
        uint32_t flushes = mFlushes.load();
        while (flushes > 0 && !mFlushes.compare_exchange_weak(flushes, flushes - 1)) {
        }
    }
    // Whether a flush still waits for the host's answer.
    bool flushing() const { return mFlushes.load() > 0; }
    // A stop or a reset: every picture so far is of a stream that has ended, and no answer to
    // a flush of it will come.
    void stop() {
        mFlushes.store(0);
        mEpoch.fetch_add(1);
    }

   private:
    std::atomic<uint64_t> mEpoch{0};
    std::atomic<uint32_t> mFlushes{0};
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

// As waitReadable(), for a surface allocator's waitable, which can stay readable while the
// allocator still has no buffer to give (a surface being replaced, as when a window moves to
// another display). `lastReady` says the wait before found it readable and the allocator
// still had none: this wait then first lasts `pause`, so the component does not ask the
// allocator again and again at once. Asking without a pause made thousands of binder calls a
// second for as long as a picture waited.
WaitResult waitForBuffer(int fd, std::chrono::steady_clock::time_point deadline, const OutputGate& gate,
                         uint64_t ticket, bool lastReady,
                         std::chrono::milliseconds slice = std::chrono::milliseconds(50),
                         std::chrono::milliseconds pause = std::chrono::milliseconds(10));

}  // namespace dsa::codec2
