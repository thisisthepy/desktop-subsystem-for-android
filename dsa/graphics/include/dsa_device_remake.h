// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// How the renderer's own Vulkan device comes back after a GPU reset without making the
// guest wait (patches/0021).
//
// On the owner's PC (Intel Arc, Windows 11, v0.2.3) the renderer took 13.4 s to make its
// device again after a reset, and it held its lock all that time: every guest call that
// touched a ColorBuffer waited, YouTube's input went unanswered for more than five seconds,
// and Android ended it. Most of that time the GPU itself was not back yet: the old objects
// were freed within the first six seconds, and the window manager's own Direct3D device
// could be made only 26 ms after the renderer's.
//
// Now the device is made again on a thread of its own. Until it is back, an operation that
// needs the device fails at once (the guest gets VK_ERROR_DEVICE_LOST, or a failed
// ColorBuffer call), instead of waiting. The lock is taken only for the short steps: freeing
// the old objects, putting the new device in place, and making each ColorBuffer again one
// at a time.
//
// Every wait for the GPU while a lock is held is bounded too: a fence is waited for in short
// slices, never past kGpuWaitBudget in all, and gives up at once when another thread found
// the device lost. Windows resets a GPU whose work runs past two seconds, so a wait past
// that only holds the lock for a GPU that is gone.

#pragma once

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>

namespace dsa {

// The longest the renderer waits for one GPU job, and the slice it waits in.
inline constexpr uint64_t kGpuWaitBudgetNs = 3000ull * 1000ull * 1000ull;
inline constexpr uint64_t kGpuWaitSliceNs = 50ull * 1000ull * 1000ull;

// How long the check of a device another loss made suspect may wait for the GPU.
inline constexpr uint64_t kSuspectProbeNs = 100ull * 1000ull * 1000ull;

// What one wait for a fence gave.
enum class GpuWait { Signaled, Timeout, DeviceLost, Failed, GivenUp };

// Waits for a fence with `wait(timeout_ns)`, which returns a GpuWait, in slices of `slice_ns`
// and never for more than `budget_ns` in all. Between slices `given_up()` is asked; when it
// says the device is lost, the wait ends with GivenUp.
template <typename Wait, typename GivenUp>
GpuWait wait_bounded(Wait&& wait, GivenUp&& given_up, uint64_t budget_ns = kGpuWaitBudgetNs,
                     uint64_t slice_ns = kGpuWaitSliceNs) {
    uint64_t waited = 0;
    for (;;) {
        if (given_up()) {
            return GpuWait::GivenUp;
        }
        const uint64_t slice = std::min(slice_ns, budget_ns - waited);
        const GpuWait got = wait(slice);
        if (got != GpuWait::Timeout) {
            return got;
        }
        waited += slice;
        if (waited >= budget_ns) {
            return GpuWait::Timeout;
        }
    }
}

// How long the remaking thread waits before it tries to make the device again after
// `failures` failed tries: the GPU may not be back from its reset yet.
inline std::chrono::milliseconds remake_retry_delay(uint32_t failures) {
    const uint32_t shift = std::min<uint32_t>(failures, 5);
    return std::chrono::milliseconds(std::min<uint32_t>(50u << shift, 1000u));
}

// The renderer's device, as the operations on it see it: usable, lost, or being made again.
class DeviceRemake {
   public:
    // The device was found lost. Returns whether this is a new loss (counted once).
    bool lost() {
        if (lost_.exchange(true)) {
            return false;
        }
        losses_.fetch_add(1);
        return true;
    }

    bool isLost() const { return lost_.load(); }

    // Whether the caller is the one to start making the device again: it is lost and nobody
    // makes it yet.
    bool claim() {
        if (!lost_.load()) {
            return false;
        }
        bool expected = false;
        return remaking_.compare_exchange_strong(expected, true);
    }

    bool remaking() const { return remaking_.load(); }

    // Whether an operation may use the device now.
    bool usable() const { return !lost_.load() && !remaking_.load(); }

    // The remaking thread is done. With `made`, the device is usable again and a new
    // generation begins; otherwise it stays lost, and the next claim tries again.
    void finished(bool made) {
        if (made) {
            generation_.fetch_add(1);
            lost_.store(false);
        }
        remaking_.store(false);
    }

    // How many times the device was found lost, and how many times it was made again.
    uint64_t losses() const { return losses_.load(); }
    uint64_t generation() const { return generation_.load(); }

   private:
    std::atomic<bool> lost_{false};
    std::atomic<bool> remaking_{false};
    std::atomic<uint64_t> losses_{0};
    std::atomic<uint64_t> generation_{0};
};

}  // namespace dsa
