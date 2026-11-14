// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The renderer's device after a GPU reset: operations fail at once while it is made again,
// one thread makes it, and no wait for the GPU outlasts its budget or a loss another thread
// found.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <utility>
#include <vector>

#include "dsa_device_remake.h"

namespace {

int failures = 0;

void check(bool ok, const char* test, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "%s: %s\n", test, what);
        failures++;
    }
}

/* A fence that answers each wait with the next answer, then with Timeout, and notes the
 * timeouts it was given. */
struct FakeFence {
    FakeFence() = default;
    explicit FakeFence(std::vector<dsa::GpuWait> given) : answers(std::move(given)) {}

    std::vector<dsa::GpuWait> answers;
    std::vector<uint64_t> timeouts;
    size_t next = 0;

    dsa::GpuWait operator()(uint64_t timeout_ns) {
        timeouts.push_back(timeout_ns);
        return next < answers.size() ? answers[next++] : dsa::GpuWait::Timeout;
    }

    uint64_t total() const {
        uint64_t sum = 0;
        for (uint64_t t : timeouts) sum += t;
        return sum;
    }
};

void nfr5_a_lost_device_is_unusable_and_one_caller_makes_it_again() {
    const char* test = __func__;
    dsa::DeviceRemake device;
    check(device.usable(), test, "usable at the start");
    check(!device.claim(), test, "nothing to make while it is not lost");
    check(device.lost(), test, "a new loss");
    check(!device.lost(), test, "the same loss is counted once");
    check(device.losses() == 1, test, "one loss");
    check(!device.usable(), test, "not usable once lost");
    check(device.claim(), test, "the first caller makes it");
    check(!device.claim(), test, "no second maker while it is made");
    check(device.remaking(), test, "being made");
    check(!device.usable(), test, "not usable while being made");
    device.finished(true);
    check(device.usable(), test, "usable once made");
    check(device.generation() == 1, test, "a new generation");
}

void nfr5_a_device_that_could_not_be_made_stays_lost_and_is_tried_again() {
    const char* test = __func__;
    dsa::DeviceRemake device;
    device.lost();
    check(device.claim(), test, "claimed");
    device.finished(false);
    check(!device.usable(), test, "still not usable");
    check(device.generation() == 0, test, "no new generation");
    check(device.claim(), test, "the next caller tries again");
    device.finished(true);
    check(device.usable() && device.generation() == 1, test, "made the second time");
    check(device.losses() == 1, test, "still one loss");
}

void nfr5_a_second_reset_is_a_second_loss() {
    const char* test = __func__;
    dsa::DeviceRemake device;
    for (int i = 0; i < 2; i++) {
        device.lost();
        device.claim();
        device.finished(true);
    }
    check(device.losses() == 2 && device.generation() == 2, test, "two losses, two devices");
}

void nfr5_a_gpu_wait_never_outlasts_its_budget() {
    const char* test = __func__;
    FakeFence fence;
    const auto got = dsa::wait_bounded(fence, [] { return false; });
    check(got == dsa::GpuWait::Timeout, test, "timed out");
    check(fence.total() == dsa::kGpuWaitBudgetNs, test, "waited the budget, no more");
    for (uint64_t t : fence.timeouts) {
        check(t <= dsa::kGpuWaitSliceNs, test, "in slices");
    }
    // The renderer's old readback waited 5 s and then 10 s more while holding its lock.
    check(dsa::kGpuWaitBudgetNs <= 3000ull * 1000 * 1000, test, "three seconds at most");
}

void nfr5_a_gpu_wait_ends_at_once_when_another_thread_found_the_device_lost() {
    const char* test = __func__;
    FakeFence fence;
    int asked = 0;
    const auto got = dsa::wait_bounded(fence, [&] { return ++asked > 2; });
    check(got == dsa::GpuWait::GivenUp, test, "given up");
    check(fence.timeouts.size() == 2, test, "two slices, then no more");
}

void nfr5_a_gpu_wait_returns_what_the_fence_says() {
    const char* test = __func__;
    FakeFence done({dsa::GpuWait::Timeout, dsa::GpuWait::Signaled});
    check(dsa::wait_bounded(done, [] { return false; }) == dsa::GpuWait::Signaled, test,
          "signaled after a slice");
    FakeFence lost({dsa::GpuWait::DeviceLost});
    check(dsa::wait_bounded(lost, [] { return false; }) == dsa::GpuWait::DeviceLost, test,
          "lost");
    check(lost.timeouts.size() == 1, test, "one wait for a lost device");
}

void nfr5_making_the_device_again_is_retried_soon_and_never_less_than_once_a_second() {
    const char* test = __func__;
    check(dsa::remake_retry_delay(0).count() == 50, test, "first retry after 50 ms");
    check(dsa::remake_retry_delay(1).count() == 100, test, "then 100 ms");
    check(dsa::remake_retry_delay(30).count() == 1000, test, "at most a second");
}

}  // namespace

int main() {
    nfr5_a_lost_device_is_unusable_and_one_caller_makes_it_again();
    nfr5_a_device_that_could_not_be_made_stays_lost_and_is_tried_again();
    nfr5_a_second_reset_is_a_second_loss();
    nfr5_a_gpu_wait_never_outlasts_its_budget();
    nfr5_a_gpu_wait_ends_at_once_when_another_thread_found_the_device_lost();
    nfr5_a_gpu_wait_returns_what_the_fence_says();
    nfr5_making_the_device_again_is_retried_soon_and_never_less_than_once_a_second();
    if (failures) {
        return EXIT_FAILURE;
    }
    std::puts("device_remake_test: ok");
    return EXIT_SUCCESS;
}
