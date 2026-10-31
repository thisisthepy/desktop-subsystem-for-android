// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "vsync_thread.h"

#include <time.h>

#include <cerrno>

namespace dsa::hwc {

namespace {

int64_t nowNanos() {
    timespec ts{};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<int64_t>(ts.tv_sec) * 1000000000LL + ts.tv_nsec;
}

void sleepUntil(int64_t nanos) {
    timespec ts{static_cast<time_t>(nanos / 1000000000LL), static_cast<long>(nanos % 1000000000LL)};
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, nullptr) == EINTR) {
    }
}

}  // namespace

int64_t nextVsyncAfter(int64_t previous, int64_t now, int32_t periodNanos) {
    if (periodNanos <= 0) {
        return now;
    }
    int64_t next = previous + periodNanos;
    if (next <= now) {
        next += ((now - next) / periodNanos + 1) * periodNanos;
    }
    return next;
}

VsyncThread::VsyncThread(int32_t periodNanos, std::function<void(int64_t)> onVsync,
                         std::function<void()> onStart)
    : mPeriodNanos(periodNanos),
      mOnVsync(std::move(onVsync)),
      mOnStart(std::move(onStart)),
      mThread([this] { run(); }) {}

VsyncThread::~VsyncThread() {
    {
        std::lock_guard<std::mutex> lock(mMutex);
        mStopping = true;
    }
    mWake.notify_all();
    mThread.join();
}

void VsyncThread::setEnabled(bool enabled) {
    {
        std::lock_guard<std::mutex> lock(mMutex);
        mEnabled = enabled;
    }
    mWake.notify_all();
}

void VsyncThread::run() {
    if (mOnStart) {
        mOnStart();
    }
    int64_t last = nowNanos();
    for (;;) {
        {
            std::unique_lock<std::mutex> lock(mMutex);
            if (!mEnabled && !mStopping) {
                mWake.wait(lock, [this] { return mEnabled || mStopping; });
            }
            if (mStopping) {
                return;
            }
        }
        // On the period's grid, skipping whatever was missed while disabled or descheduled.
        const int64_t next = nextVsyncAfter(last, nowNanos(), mPeriodNanos);
        sleepUntil(next);
        last = next;
        bool enabled;
        {
            std::lock_guard<std::mutex> lock(mMutex);
            enabled = mEnabled && !mStopping;
        }
        if (enabled) {
            mOnVsync(next);
        }
    }
}

}  // namespace dsa::hwc
