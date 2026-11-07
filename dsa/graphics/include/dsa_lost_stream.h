// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What the guest Vulkan driver does when its stream to the host renderer ends.
//
// The renderer runs in a process of its own, which the window manager restarts when it ends,
// and it ends one guest's stream when that guest sends a command it cannot carry out. Either
// way the guest's socket closes. The driver used to abort the process on the first read that
// failed, and when that process was SurfaceFlinger the whole Android framework restarted.
//
// Now a stream that ended is lost: nothing more is sent on it, a read gives zeros (no handles,
// no counts), and the encoder turns the result of each call into VK_ERROR_DEVICE_LOST. That is
// what a Vulkan driver reports for a lost device, so ANGLE marks its display lost and
// SurfaceFlinger's and HWUI's renderers drop their contexts and build new ones.
//
// Building a new one goes through an instance-level call (vkCreateInstance, which
// eglInitialize makes, or the instance enumerations before it). The first such call after a
// stream was lost starts a new generation: the process registers with the renderer again and
// its sequence numbers start from zero, as a restarted renderer's do. Then every thread whose
// stream is from an older generation connects again, at most every kReconnectInterval while
// the renderer does not answer yet.

#pragma once

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

namespace dsa {

// How often a thread whose stream was lost tries to connect again while the renderer does not
// answer: a connect to a port nobody listens on can take a while to fail.
inline constexpr std::chrono::milliseconds kReconnectInterval{250};

// The process's streams to the host renderer, counted in generations.
class StreamGenerations {
   public:
    static uint32_t current() { return sCurrent.load(); }
    // Whether a stream was lost since the current generation began.
    static bool anyLost() { return sAnyLost.load(); }
    static void streamLost() { sAnyLost = true; }
    static void beginNew() {
        sAnyLost = false;
        sCurrent++;
    }

   private:
    static inline std::atomic<uint32_t> sCurrent{0};
    static inline std::atomic<bool> sAnyLost{false};
};

// One stream's state: whether the renderer still answers it, and its generation.
class LostStream {
   public:
    LostStream() : mGeneration(StreamGenerations::current()) {}

    bool lost() const { return mLost; }
    uint32_t generation() const { return mGeneration; }

    // The stream failed a read or a write. True the first time, when the caller logs it.
    bool markLost() {
        if (mLost) {
            return false;
        }
        mLost = true;
        StreamGenerations::streamLost();
        return true;
    }

    // What a read on a lost stream gives: zeros.
    static void zero(void* buffer, size_t size) {
        if (buffer && size) {
            std::memset(buffer, 0, size);
        }
    }

    // Where the encoder writes `size` bytes once nothing can be sent.
    uint8_t* scratch(size_t size) {
        if (mScratch.size() < size) {
            mScratch.resize(size);
        }
        return mScratch.data();
    }

   private:
    bool mLost = false;
    uint32_t mGeneration;
    std::vector<uint8_t> mScratch;
};

// Whether a thread whose stream is from `streamGeneration` connects again now.
inline bool shouldReconnect(bool streamLost, uint32_t streamGeneration,
                            std::chrono::steady_clock::time_point now,
                            std::chrono::steady_clock::time_point lastAttempt) {
    return streamLost && streamGeneration < StreamGenerations::current() &&
           now - lastAttempt >= kReconnectInterval;
}

}  // namespace dsa
