// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "send_queue.h"

#include <utility>

namespace dsa::codec2 {

bool SendQueue::push(std::vector<uint8_t> frame) {
    std::unique_lock<std::mutex> lock(mLock);
    // A frame larger than the bound still goes, alone.
    mChanged.wait(lock, [&] { return mClosed || mBytes == 0 || mBytes + frame.size() <= mMaxBytes; });
    if (mClosed) {
        return false;
    }
    mBytes += frame.size();
    mFrames.push_back(std::move(frame));
    mChanged.notify_all();
    return true;
}

bool SendQueue::pop(std::vector<uint8_t>* frame) {
    std::unique_lock<std::mutex> lock(mLock);
    mChanged.wait(lock, [&] { return mClosed || !mFrames.empty(); });
    if (mClosed) {
        return false;
    }
    *frame = std::move(mFrames.front());
    mFrames.pop_front();
    mBytes -= frame->size();
    mChanged.notify_all();
    return true;
}

void SendQueue::close() {
    std::lock_guard<std::mutex> lock(mLock);
    mClosed = true;
    mFrames.clear();
    mBytes = 0;
    mChanged.notify_all();
}

size_t SendQueue::queuedBytes() {
    std::lock_guard<std::mutex> lock(mLock);
    return mBytes;
}

}  // namespace dsa::codec2
