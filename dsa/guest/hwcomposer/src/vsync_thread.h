// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The display's vsync: the guest display has no hardware behind it, so a clock stands in,
// ticking at the display's refresh rate while SurfaceFlinger wants vsync events.

#pragma once

#include <condition_variable>
#include <cstdint>
#include <functional>
#include <mutex>
#include <thread>

namespace dsa::hwc {

class VsyncThread {
   public:
    // `onVsync` gets each tick's CLOCK_MONOTONIC time in nanoseconds.
    VsyncThread(int32_t periodNanos, std::function<void(int64_t)> onVsync);
    ~VsyncThread();

    void setEnabled(bool enabled);

   private:
    void run();

    const int32_t mPeriodNanos;
    const std::function<void(int64_t)> mOnVsync;
    std::mutex mMutex;
    std::condition_variable mWake;
    bool mEnabled = false;
    bool mStopping = false;
    std::thread mThread;
};

}  // namespace dsa::hwc
