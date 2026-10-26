// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Runs the host's work for a frame off SurfaceFlinger's thread, one frame at a time.
//
// SurfaceFlinger hands a layer's previous buffer back to its app when the present after the
// one that showed it returns, and the release fences this composer gives it are empty. So a
// frame's buffers must be read by the host before the next present returns, not before its
// own present returns. submit() therefore returns at once and waits only for the frame
// before it: SurfaceFlinger latches, composes and presents the next frame while the host
// composes this one, instead of waiting out every round trip to the host on its main thread.

#pragma once

#include <condition_variable>
#include <functional>
#include <mutex>
#include <thread>

namespace dsa::hwc {

class FramePipeline {
   public:
    using Job = std::function<void()>;

    // onStart runs once on the worker thread before any job.
    explicit FramePipeline(std::function<void()> onStart = {});
    // Finishes the frame in flight, then stops.
    ~FramePipeline();

    FramePipeline(const FramePipeline&) = delete;
    FramePipeline& operator=(const FramePipeline&) = delete;

    // Waits until the frame before has been done, then hands `job` to the worker and returns
    // without waiting for it.
    void submit(Job job);
    // Returns once no frame is in flight.
    void drain();

   private:
    void run();

    std::mutex mMutex;
    std::condition_variable mWake;
    std::condition_variable mIdle;
    Job mJob;
    // A job is queued or running.
    bool mBusy = false;
    bool mStopping = false;
    std::function<void()> mOnStart;
    // Last, so it starts once everything above exists.
    std::thread mThread;
};

}  // namespace dsa::hwc
