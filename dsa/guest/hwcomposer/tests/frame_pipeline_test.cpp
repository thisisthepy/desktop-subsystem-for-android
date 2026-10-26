// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The present path: SurfaceFlinger's present does not wait for the host to compose the
// frame, and the host has finished with a frame before the next present returns, which is
// when SurfaceFlinger gives that frame's buffers back to their apps.

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <mutex>
#include <thread>
#include <vector>

#include "frame_pipeline.h"
#include "test_main.h"

using namespace dsa::hwc;

namespace {

// A host that answers only when the test lets it.
struct Gate {
    std::mutex mutex;
    std::condition_variable changed;
    bool open = false;

    void wait() {
        std::unique_lock<std::mutex> lock(mutex);
        changed.wait(lock, [this] { return open; });
    }
    void release() {
        {
            std::lock_guard<std::mutex> lock(mutex);
            open = true;
        }
        changed.notify_all();
    }
};

}  // namespace

TEST(fr5_present_returns_before_the_host_has_composed_the_frame) {
    FramePipeline pipeline;
    Gate host;
    std::atomic<bool> composed{false};
    const auto started = std::chrono::steady_clock::now();
    pipeline.submit([&] {
        host.wait();
        composed = true;
    });
    // SurfaceFlinger's thread is free again while the host still works on the frame.
    EXPECT_TRUE(!composed);
    EXPECT_TRUE(std::chrono::steady_clock::now() - started < std::chrono::milliseconds(500));
    host.release();
    pipeline.drain();
    EXPECT_TRUE(composed);
}

TEST(fr5_the_next_present_waits_until_the_host_is_done_with_the_previous_frame) {
    // The next present returning is SurfaceFlinger's signal to give the previous frame's
    // buffers back to their apps, so the host must have read them by then.
    FramePipeline pipeline;
    Gate host;
    std::atomic<bool> firstDone{false};
    std::atomic<bool> secondSubmitted{false};
    pipeline.submit([&] {
        host.wait();
        firstDone = true;
    });
    std::thread next([&] {
        pipeline.submit([] {});
        // The first frame was finished before this present could hand over its own.
        EXPECT_TRUE(firstDone);
        secondSubmitted = true;
    });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    EXPECT_TRUE(!secondSubmitted);
    host.release();
    next.join();
    EXPECT_TRUE(secondSubmitted);
    pipeline.drain();
}

TEST(fr5_frames_reach_the_host_in_present_order) {
    FramePipeline pipeline;
    std::mutex mutex;
    std::vector<int> order;
    for (int i = 0; i < 20; ++i) {
        pipeline.submit([&, i] {
            std::lock_guard<std::mutex> lock(mutex);
            order.push_back(i);
        });
    }
    pipeline.drain();
    EXPECT_EQ(order.size(), 20u);
    for (int i = 0; i < static_cast<int>(order.size()); ++i) {
        EXPECT_EQ(order[static_cast<size_t>(i)], i);
    }
}

TEST(fr5_a_frame_in_flight_still_goes_out_when_the_composer_stops) {
    std::atomic<bool> composed{false};
    {
        FramePipeline pipeline;
        pipeline.submit([&] {
            std::this_thread::sleep_for(std::chrono::milliseconds(20));
            composed = true;
        });
    }
    EXPECT_TRUE(composed);
}

TEST(fr5_the_host_work_runs_on_a_thread_set_up_by_on_start) {
    std::atomic<std::thread::id> started{};
    std::atomic<std::thread::id> ran{};
    {
        FramePipeline pipeline([&] { started = std::this_thread::get_id(); });
        pipeline.submit([&] { ran = std::this_thread::get_id(); });
        pipeline.drain();
    }
    EXPECT_TRUE(started.load() == ran.load());
    EXPECT_TRUE(ran.load() != std::this_thread::get_id());
}

int main() { return dsa::test::runAll(); }
