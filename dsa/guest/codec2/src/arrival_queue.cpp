// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "arrival_queue.h"

#include <utility>
#include <variant>

namespace dsa::codec2 {

size_t ArrivalQueue::bytesOf(const Arrival& arrival) {
    const auto* frame = std::get_if<Frame>(&arrival.message);
    return frame ? frame->nv12.size() : 0;
}

bool ArrivalQueue::push(Arrival arrival) {
    const size_t bytes = bytesOf(arrival);
    std::unique_lock<std::mutex> lock(mLock);
    mChanged.wait(lock, [&] {
        return mClosed || mQueued.empty() || (mQueued.size() < mMaxCount && mBytes + bytes <= mMaxBytes);
    });
    if (mClosed) {
        return false;
    }
    mBytes += bytes;
    mQueued.push_back(std::move(arrival));
    mChanged.notify_all();
    return true;
}

bool ArrivalQueue::pop(Arrival* arrival) {
    std::unique_lock<std::mutex> lock(mLock);
    mChanged.wait(lock, [&] { return mClosed || !mQueued.empty(); });
    if (mClosed) {
        return false;
    }
    *arrival = std::move(mQueued.front());
    mQueued.pop_front();
    mBytes -= bytesOf(*arrival);
    mChanged.notify_all();
    return true;
}

void ArrivalQueue::close() {
    std::lock_guard<std::mutex> lock(mLock);
    mClosed = true;
    mQueued.clear();
    mBytes = 0;
    mChanged.notify_all();
}

void ArrivalQueue::open() {
    std::lock_guard<std::mutex> lock(mLock);
    mClosed = false;
    mQueued.clear();
    mBytes = 0;
    mChanged.notify_all();
}

size_t ArrivalQueue::size() {
    std::lock_guard<std::mutex> lock(mLock);
    return mQueued.size();
}

}  // namespace dsa::codec2
