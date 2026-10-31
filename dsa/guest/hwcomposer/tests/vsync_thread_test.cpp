// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The display's vsync clock: its ticks stay on the period's grid, come on time, and reach
// SurfaceFlinger without waiting on anything else in the composer.

#include <time.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <mutex>
#include <thread>
#include <vector>

#include "test_main.h"
#include "vsync_thread.h"

using namespace dsa::hwc;

namespace {

constexpr int32_t kPeriod = 16666666;  // 60 Hz

int64_t monotonicNanos() {
    timespec ts{};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<int64_t>(ts.tv_sec) * 1000000000LL + ts.tv_nsec;
}

}  // namespace

TEST(fr5_next_vsync_is_one_period_after_the_last_when_on_time) {
    EXPECT_EQ(nextVsyncAfter(1000, 1000 + 5, kPeriod), 1000 + kPeriod);
}

TEST(fr5_missed_vsyncs_are_skipped_not_replayed) {
    // The thread was off the CPU for a second: the next tick is the first grid point ahead
    // of now, so SurfaceFlinger does not get sixty stale ticks in a burst.
    const int64_t last = 1000;
    const int64_t now = last + 1000000000LL;
    const int64_t next = nextVsyncAfter(last, now, kPeriod);
    EXPECT_TRUE(next > now);
    EXPECT_TRUE(next - now <= kPeriod);
    EXPECT_EQ((next - last) % kPeriod, 0);
}

TEST(fr5_a_now_exactly_on_the_grid_gives_the_following_tick) {
    EXPECT_EQ(nextVsyncAfter(0, 3LL * kPeriod, kPeriod), 4LL * kPeriod);
}

TEST(fr5_vsync_ticks_carry_monotonic_times_at_the_display_rate) {
    std::mutex mutex;
    std::condition_variable done;
    std::vector<int64_t> ticks;
    std::vector<int64_t> lateness;
    VsyncThread vsync(kPeriod, [&](int64_t timestamp) {
        const int64_t now = monotonicNanos();
        std::lock_guard<std::mutex> lock(mutex);
        ticks.push_back(timestamp);
        lateness.push_back(now - timestamp);
        done.notify_all();
    });
    vsync.setEnabled(true);
    {
        std::unique_lock<std::mutex> lock(mutex);
        done.wait_for(lock, std::chrono::seconds(5), [&] { return ticks.size() >= 10; });
    }
    vsync.setEnabled(false);
    std::lock_guard<std::mutex> lock(mutex);
    EXPECT_TRUE(ticks.size() >= 10);
    for (size_t i = 1; i < ticks.size(); ++i) {
        // On the grid: a whole number of periods apart, and moving forward.
        EXPECT_TRUE(ticks[i] > ticks[i - 1]);
        EXPECT_EQ((ticks[i] - ticks[i - 1]) % kPeriod, 0);
    }
    for (int64_t late : lateness) {
        // A tick is never stamped in the future, and the timestamp is the clock SurfaceFlinger
        // reads (CLOCK_MONOTONIC), not some other base.
        EXPECT_TRUE(late >= 0);
        EXPECT_TRUE(late < 1000000000LL);
    }
}

TEST(fr5_vsync_callback_runs_with_no_lock_held) {
    // SurfaceFlinger may switch vsync off from the very thread a tick arrives on (a callback
    // that re-enters the composer); that must not wait on the vsync thread's own lock.
    std::atomic<int> calls{0};
    VsyncThread* self = nullptr;
    std::mutex mutex;
    std::condition_variable done;
    VsyncThread vsync(kPeriod, [&](int64_t) {
        self->setEnabled(false);
        self->setEnabled(true);
        ++calls;
        done.notify_all();
    });
    self = &vsync;
    vsync.setEnabled(true);
    std::unique_lock<std::mutex> lock(mutex);
    done.wait_for(lock, std::chrono::seconds(5), [&] { return calls.load() >= 3; });
    EXPECT_TRUE(calls.load() >= 3);
    vsync.setEnabled(false);
}

TEST(fr5_a_slow_vsync_consumer_does_not_hold_back_set_enabled) {
    // A tick that takes long to deliver must not stall the composer's binder threads.
    std::atomic<bool> inCallback{false};
    std::atomic<bool> release{false};
    VsyncThread vsync(kPeriod, [&](int64_t) {
        inCallback = true;
        while (!release) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
    });
    vsync.setEnabled(true);
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (!inCallback && std::chrono::steady_clock::now() < deadline) {
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    EXPECT_TRUE(inCallback.load());
    const auto before = std::chrono::steady_clock::now();
    vsync.setEnabled(false);
    const auto took = std::chrono::steady_clock::now() - before;
    EXPECT_TRUE(took < std::chrono::milliseconds(100));
    release = true;
}

TEST(fr5_vsync_on_start_runs_on_the_vsync_thread_before_any_tick) {
    // The owner raises the thread to real-time priority there, so it must be the vsync
    // thread itself and it must come first.
    std::mutex mutex;
    std::condition_variable done;
    std::thread::id startThread;
    std::thread::id tickThread;
    bool startedBeforeTick = false;
    bool started = false;
    bool ticked = false;
    VsyncThread vsync(
        kPeriod,
        [&](int64_t) {
            std::lock_guard<std::mutex> lock(mutex);
            if (!ticked) {
                tickThread = std::this_thread::get_id();
                startedBeforeTick = started;
                ticked = true;
            }
            done.notify_all();
        },
        [&] {
            std::lock_guard<std::mutex> lock(mutex);
            startThread = std::this_thread::get_id();
            started = true;
        });
    vsync.setEnabled(true);
    std::unique_lock<std::mutex> lock(mutex);
    done.wait_for(lock, std::chrono::seconds(5), [&] { return ticked; });
    EXPECT_TRUE(ticked);
    EXPECT_TRUE(startedBeforeTick);
    EXPECT_TRUE(startThread == tickThread);
    EXPECT_TRUE(startThread != std::this_thread::get_id());
    lock.unlock();
    vsync.setEnabled(false);
}

int main() { return dsa::test::runAll(); }
