// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "picture_stats.h"

#include <cstdio>

namespace dsa::codec2 {

void PictureStats::begin(Clock::time_point now) {
    if (!mStarted) {
        mStarted = true;
        mStart = now;
    }
}

void PictureStats::received(Clock::time_point now) {
    std::lock_guard<std::mutex> lock(mLock);
    begin(now);
    ++mReceived;
}

void PictureStats::shown(Clock::time_point now, const PictureTimes& times) {
    std::lock_guard<std::mutex> lock(mLock);
    begin(now);
    ++mShown;
    mTotal.queued += times.queued;
    mTotal.buffer += times.buffer;
    mTotal.write += times.write;
    mTotal.handOff += times.handOff;
}

void PictureStats::dropped(Clock::time_point now, Drop why) {
    std::lock_guard<std::mutex> lock(mLock);
    begin(now);
    switch (why) {
        case Drop::Flushed:
            ++mFlushed;
            break;
        case Drop::NoBuffer:
            ++mNoBuffer;
            break;
        case Drop::NotWritten:
            ++mNotWritten;
            break;
    }
}

bool PictureStats::report(Clock::time_point now, size_t waiting, std::string* line) {
    std::lock_guard<std::mutex> lock(mLock);
    if (!mStarted || now < mStart + mInterval) {
        return false;
    }
    const double seconds = std::chrono::duration<double>(now - mStart).count();
    const auto average = [this](std::chrono::microseconds total) {
        return mShown == 0 ? 0.0 : static_cast<double>(total.count()) / 1000.0 / mShown;
    };
    char text[512];
    const int length = std::snprintf(text, sizeof(text),
                  "over %.1f s: %u pictures from the host (%.1f/s), %u shown (%.1f/s), %u dropped (%u flushed, %u "
                  "with no buffer, %u not written); average ms: queued %.2f, buffer wait %.2f, write %.2f, hand-off "
                  "%.2f; %zu waiting",
                  seconds, mReceived, mReceived / seconds, mShown, mShown / seconds, mFlushed + mNoBuffer + mNotWritten,
                  mFlushed, mNoBuffer, mNotWritten, average(mTotal.queued), average(mTotal.buffer),
                  average(mTotal.write), average(mTotal.handOff), waiting);
    if (length < 0) {
        return false;
    }
    *line = text;
    mStart = now;
    mReceived = 0;
    mShown = 0;
    mFlushed = 0;
    mNoBuffer = 0;
    mNotWritten = 0;
    mTotal = PictureTimes{};
    return true;
}

}  // namespace dsa::codec2
