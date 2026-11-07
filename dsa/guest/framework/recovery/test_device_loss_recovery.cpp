/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

// Tests of the recovery policy SurfaceFlinger's RenderEngine and HWUI's RenderThread share
// (DeviceLossRecovery.h, which the framework patches add to each). It needs no Android: build
// and run it with any C++17 compiler, as scripts/tests/framework.test.sh does.

#include "DeviceLossRecovery.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <string>
#include <vector>

using android::gpurecovery::DeviceLossRecovery;
using namespace std::chrono_literals;

namespace {

int gFailures = 0;

void expect(bool condition, const char* what, int line) {
    if (!condition) {
        std::fprintf(stderr, "  line %d: expected %s\n", line, what);
        gFailures++;
    }
}

#define EXPECT(condition) expect((condition), #condition, __LINE__)

const DeviceLossRecovery::Clock::time_point kStart{};

void nfr5_a_healthy_renderer_never_rebuilds() {
    DeviceLossRecovery recovery;
    EXPECT(recovery.isHealthy());
    EXPECT(!recovery.shouldRebuild(kStart));
    EXPECT(recovery.untilNextAttempt(kStart) == DeviceLossRecovery::Clock::duration::zero());
    // A result that arrives while healthy changes nothing.
    recovery.onRebuilt(true, kStart);
    EXPECT(recovery.resetCount() == 0);
}

void nfr5_a_loss_is_handled_once_and_rebuilt_at_once() {
    DeviceLossRecovery recovery;
    EXPECT(recovery.onLost(kStart));
    // Every user of the lost context sees the loss; only the first drops what it made.
    EXPECT(!recovery.onLost(kStart + 1ms));
    EXPECT(recovery.state() == DeviceLossRecovery::State::Lost);
    EXPECT(recovery.shouldRebuild(kStart));
    recovery.onRebuilt(true, kStart);
    EXPECT(recovery.isHealthy());
    EXPECT(recovery.resetCount() == 1);
    EXPECT(recovery.failedAttempts() == 0);
}

void nfr5_attempts_back_off_while_the_gpu_does_not_answer() {
    DeviceLossRecovery recovery;
    recovery.onLost(kStart);
    recovery.onRebuilt(false, kStart);
    EXPECT(recovery.state() == DeviceLossRecovery::State::Waiting);
    EXPECT(!recovery.shouldRebuild(kStart + 49ms));
    EXPECT(recovery.shouldRebuild(kStart + 50ms));
    EXPECT(recovery.untilNextAttempt(kStart + 20ms) == 30ms);

    auto now = kStart + 50ms;
    recovery.onRebuilt(false, now);
    EXPECT(recovery.failedAttempts() == 2);
    EXPECT(!recovery.shouldRebuild(now + 99ms));
    EXPECT(recovery.shouldRebuild(now + 100ms));
}

void nfr5_the_wait_never_grows_past_the_longest_retry_and_never_gives_up() {
    EXPECT(DeviceLossRecovery::retryDelay(1) == DeviceLossRecovery::kFirstRetry);
    EXPECT(DeviceLossRecovery::retryDelay(2) == 100ms);
    EXPECT(DeviceLossRecovery::retryDelay(6) == DeviceLossRecovery::kLongestRetry);
    EXPECT(DeviceLossRecovery::retryDelay(1000) == DeviceLossRecovery::kLongestRetry);

    DeviceLossRecovery recovery;
    recovery.onLost(kStart);
    auto now = kStart;
    // A TDR on the host takes seconds; a minute of failed attempts still leads to a rebuild.
    while (now < kStart + 60s) {
        EXPECT(recovery.shouldRebuild(now));
        recovery.onRebuilt(false, now);
        now += recovery.untilNextAttempt(now);
    }
    EXPECT(recovery.untilNextAttempt(now - 1ms) <= DeviceLossRecovery::kLongestRetry);
    recovery.onRebuilt(true, now);
    EXPECT(recovery.isHealthy());
    EXPECT(recovery.resetCount() == 1);
}

void nfr5_each_loss_counts_one_reset_for_the_caches() {
    DeviceLossRecovery recovery;
    for (int i = 0; i < 3; i++) {
        recovery.onLost(kStart);
        recovery.onRebuilt(false, kStart);
        recovery.onRebuilt(true, kStart + 50ms);
    }
    // A cache of earlier drawing compares this with the count it saw last.
    EXPECT(recovery.resetCount() == 3);
    // A new loss starts the backoff over.
    recovery.onLost(kStart);
    recovery.onRebuilt(false, kStart);
    EXPECT(recovery.failedAttempts() == 1);
    EXPECT(recovery.untilNextAttempt(kStart) == DeviceLossRecovery::kFirstRetry);
}

}  // namespace

int main() {
    const std::vector<std::pair<std::string, std::function<void()>>> tests = {
            {"nfr5_a_healthy_renderer_never_rebuilds", nfr5_a_healthy_renderer_never_rebuilds},
            {"nfr5_a_loss_is_handled_once_and_rebuilt_at_once",
             nfr5_a_loss_is_handled_once_and_rebuilt_at_once},
            {"nfr5_attempts_back_off_while_the_gpu_does_not_answer",
             nfr5_attempts_back_off_while_the_gpu_does_not_answer},
            {"nfr5_the_wait_never_grows_past_the_longest_retry_and_never_gives_up",
             nfr5_the_wait_never_grows_past_the_longest_retry_and_never_gives_up},
            {"nfr5_each_loss_counts_one_reset_for_the_caches",
             nfr5_each_loss_counts_one_reset_for_the_caches},
    };
    for (const auto& [name, test] : tests) {
        const int before = gFailures;
        test();
        std::printf("%s %s\n", gFailures == before ? "ok  " : "FAIL", name.c_str());
    }
    if (gFailures) {
        std::fprintf(stderr, "%d expectations failed\n", gFailures);
        return EXIT_FAILURE;
    }
    return EXIT_SUCCESS;
}
