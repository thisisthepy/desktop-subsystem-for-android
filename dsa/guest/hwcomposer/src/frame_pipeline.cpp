// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "frame_pipeline.h"

#include <utility>

namespace dsa::hwc {

FramePipeline::FramePipeline(std::function<void()> onStart)
    : mOnStart(std::move(onStart)), mThread([this] { run(); }) {}

FramePipeline::~FramePipeline() {
    {
        std::lock_guard<std::mutex> lock(mMutex);
        mStopping = true;
    }
    mWake.notify_all();
    mThread.join();
}

void FramePipeline::submit(Job job) {
    std::unique_lock<std::mutex> lock(mMutex);
    mIdle.wait(lock, [this] { return !mBusy; });
    mJob = std::move(job);
    mBusy = true;
    mWake.notify_all();
}

void FramePipeline::drain() {
    std::unique_lock<std::mutex> lock(mMutex);
    mIdle.wait(lock, [this] { return !mBusy; });
}

void FramePipeline::run() {
    if (mOnStart) {
        mOnStart();
    }
    std::unique_lock<std::mutex> lock(mMutex);
    for (;;) {
        // A job already handed over is done even when stopping, so its frame still goes out.
        mWake.wait(lock, [this] { return mStopping || mBusy; });
        if (!mBusy) {
            return;
        }
        Job job = std::move(mJob);
        mJob = nullptr;
        lock.unlock();
        if (job) {
            job();
        }
        lock.lock();
        mBusy = false;
        mIdle.notify_all();
    }
}

}  // namespace dsa::hwc
