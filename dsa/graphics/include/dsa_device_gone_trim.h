// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// When the renderer gives back what a guest's Vulkan device left behind.
//
// An app that closes destroys its Vulkan device in the renderer, and its device's memory goes
// with it. What the renderer's heaps and the driver kept around it does not: freed blocks stay
// committed in the heap, and the renderer's command pool keeps the memory its command buffers
// grew to. The renderer compacted its heaps only when no task window was left, so with any
// window still open about 18 MiB stayed after one app opened and closed (the dev.37 report).
//
// So a device that goes away schedules one trim: the heaps are compacted and the command pool
// is trimmed. It waits until no device has gone for a moment, so an app whose processes end
// together (an app and its widget host, or a system restart of several apps) costs one trim,
// and the compaction never runs on the thread that destroys the device. The working set is
// left alone: windows are open, and their pages must not be paged out under them.

#pragma once

#include <chrono>

namespace dsa {

class DeviceGoneTrim {
   public:
    using Clock = std::chrono::steady_clock;

    // How long after the last device went away the trim waits.
    static constexpr std::chrono::milliseconds kSettle{2000};

    // A guest's device went away at `now`.
    void deviceGone(Clock::time_point now) {
        mGone++;
        mLast = now;
        mPending = true;
    }

    // Whether the trim is due at `now`: once after the devices stopped going away for
    // kSettle. The caller then trims; until another device goes, it is not due again.
    bool due(Clock::time_point now) {
        if (!mPending || now - mLast < kSettle) {
            return false;
        }
        mPending = false;
        return true;
    }

    // How many devices went away since the last trim, which the trim's log line says; reset by
    // the call.
    unsigned takeGone() {
        const unsigned gone = mGone;
        mGone = 0;
        return gone;
    }

   private:
    Clock::time_point mLast{};
    unsigned mGone = 0;
    bool mPending = false;
};

}  // namespace dsa
