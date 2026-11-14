// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// One decoder's numbers over a few seconds, for a line in the log: how many pictures came
// from the host and how many went to the app, why the others were dropped, and where each
// picture's time went on the guest. A stream that plays slowly then says which stage holds it
// up; the host logs its own stages (decode, the copy out of its GPU, the send) the same way.

#pragma once

#include <chrono>
#include <cstdint>
#include <mutex>
#include <string>

namespace dsa::codec2 {

// Why a picture never reached the app.
enum class Drop {
    // A flush, a stop or a reset came after it: it belongs to the stream before.
    Flushed,
    // No buffer of the app's surface came back in time.
    NoBuffer,
    // A buffer came, but the picture could not be written into it.
    NotWritten,
    // The host did not decode its packet: it came after a flush or the stream's start and
    // before a key frame, and its picture would have shown green.
    BeforeKeyFrame,
};

// Where one picture's time went, from the moment it came from the host.
struct PictureTimes {
    // Waiting behind the pictures before it.
    std::chrono::microseconds queued{0};
    // Waiting for a buffer of the surface, and for its consumer to let go of it.
    std::chrono::microseconds buffer{0};
    // Writing the picture into the buffer.
    std::chrono::microseconds write{0};
    // Handing the finished work to the framework.
    std::chrono::microseconds handOff{0};
};

class PictureStats {
   public:
    using Clock = std::chrono::steady_clock;

    explicit PictureStats(std::chrono::milliseconds interval = std::chrono::seconds(5)) : mInterval(interval) {}

    // A picture came from the host.
    void received(Clock::time_point now);
    // A picture went to the app, its stages having taken `times`.
    void shown(Clock::time_point now, const PictureTimes& times);
    // A picture was dropped.
    void dropped(Clock::time_point now, Drop why);

    // Once the interval has passed since the count began, the line for it in `line` (without
    // the decoder's name) and true, and a new count from `now`; false before then and while
    // nothing has happened. `waiting` is how many pictures wait now.
    bool report(Clock::time_point now, size_t waiting, std::string* line);

   private:
    void begin(Clock::time_point now);

    const std::chrono::milliseconds mInterval;
    std::mutex mLock;
    bool mStarted = false;
    Clock::time_point mStart;
    uint32_t mReceived = 0;
    uint32_t mShown = 0;
    uint32_t mFlushed = 0;
    uint32_t mNoBuffer = 0;
    uint32_t mNotWritten = 0;
    uint32_t mBeforeKeyFrame = 0;
    PictureTimes mTotal;
};

}  // namespace dsa::codec2
