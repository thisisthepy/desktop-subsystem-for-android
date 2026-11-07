/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

#pragma once

#include <algorithm>
#include <chrono>
#include <cstdint>

namespace android {
namespace gpurecovery {

// When a renderer builds its GPU context again after the device was lost (VK_ERROR_DEVICE_LOST,
// EGL_CONTEXT_LOST). The renderer says what it sees and how each attempt went; this says when to
// try. While the host is still resetting its GPU an attempt fails, so the attempts back off from
// kFirstRetry to kLongestRetry and then go on at that pace. The renderer never gives up: the GPU
// comes back, and a renderer that stopped trying would stay blank until its process restarted.
class DeviceLossRecovery {
public:
    using Clock = std::chrono::steady_clock;

    static constexpr std::chrono::milliseconds kFirstRetry{50};
    static constexpr std::chrono::milliseconds kLongestRetry{1600};

    enum class State {
        // The context works, or there is none yet and nothing was lost.
        Healthy,
        // The device was lost and nothing has been tried yet: build the context again now.
        Lost,
        // An attempt failed; the next one waits until the retry delay has passed.
        Waiting,
    };

    State state() const { return mState; }
    bool isHealthy() const { return mState == State::Healthy; }

    // How many times the context was built again after a loss. A cache of what an earlier
    // context drew keeps the count it saw, and starts over when the count moves on.
    uint32_t resetCount() const { return mResets; }

    // Failed attempts since the device was lost.
    uint32_t failedAttempts() const { return mFailedAttempts; }

    // The device was found lost. True the first time, when the renderer must drop everything
    // the lost context made; false when the loss is already being handled.
    bool onLost(Clock::time_point now) {
        if (mState != State::Healthy) {
            return false;
        }
        mState = State::Lost;
        mFailedAttempts = 0;
        mNextAttempt = now;
        return true;
    }

    // Whether an attempt to build the context again may be made now.
    bool shouldRebuild(Clock::time_point now) const {
        return mState == State::Lost || (mState == State::Waiting && now >= mNextAttempt);
    }

    // How long until the next attempt may be made: zero when it may be made now, or when
    // nothing needs building.
    Clock::duration untilNextAttempt(Clock::time_point now) const {
        if (mState != State::Waiting || now >= mNextAttempt) {
            return Clock::duration::zero();
        }
        return mNextAttempt - now;
    }

    // How an attempt went. A success makes the renderer healthy and counts a reset; a failure
    // schedules the next attempt.
    void onRebuilt(bool succeeded, Clock::time_point now) {
        if (mState == State::Healthy) {
            return;
        }
        if (succeeded) {
            mState = State::Healthy;
            mFailedAttempts = 0;
            mResets++;
            return;
        }
        mFailedAttempts++;
        mState = State::Waiting;
        mNextAttempt = now + retryDelay(mFailedAttempts);
    }

    // The wait after the given number of failed attempts: kFirstRetry after the first, doubling
    // after each further one, and never more than kLongestRetry.
    static Clock::duration retryDelay(uint32_t failedAttempts) {
        Clock::duration delay = kFirstRetry;
        for (uint32_t i = 1; i < failedAttempts && delay < kLongestRetry; i++) {
            delay *= 2;
        }
        return std::min<Clock::duration>(delay, kLongestRetry);
    }

private:
    State mState = State::Healthy;
    uint32_t mResets = 0;
    uint32_t mFailedAttempts = 0;
    Clock::time_point mNextAttempt{};
};

}  // namespace gpurecovery
}  // namespace android
