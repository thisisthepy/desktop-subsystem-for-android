// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "send_queue.h"

#include <utility>

namespace dsa::codec2 {

void SendQueue::add(std::vector<uint8_t> frame, bool droppable) {
    mBytes += frame.size();
    mFrames.push_back(Queued{std::move(frame), droppable});
    mChanged.notify_all();
}

bool SendQueue::push(std::vector<uint8_t> frame) {
    std::unique_lock<std::mutex> lock(mLock);
    // A frame larger than the bound still goes, alone.
    mChanged.wait(lock, [&] { return mClosed || hasRoom(frame.size()); });
    if (mClosed) {
        return false;
    }
    add(std::move(frame), false);
    return true;
}

PushResult SendQueue::pushFor(std::vector<uint8_t> frame, bool droppable, std::chrono::milliseconds wait) {
    std::unique_lock<std::mutex> lock(mLock);
    const bool ready = mChanged.wait_for(lock, wait, [&] { return mClosed || hasRoom(frame.size()); });
    if (mClosed) {
        return PushResult::Closed;
    }
    if (!ready) {
        return PushResult::Full;
    }
    add(std::move(frame), droppable);
    return PushResult::Queued;
}

int SendQueue::pushReplacingDroppable(std::vector<uint8_t> frame) {
    std::lock_guard<std::mutex> lock(mLock);
    if (mClosed) {
        return -1;
    }
    int dropped = 0;
    for (auto it = mFrames.begin(); it != mFrames.end();) {
        if (it->droppable) {
            mBytes -= it->bytes.size();
            it = mFrames.erase(it);
            ++dropped;
        } else {
            ++it;
        }
    }
    add(std::move(frame), false);
    return dropped;
}

bool SendQueue::pop(std::vector<uint8_t>* frame) {
    std::unique_lock<std::mutex> lock(mLock);
    mChanged.wait(lock, [&] { return mClosed || !mFrames.empty(); });
    if (mClosed) {
        return false;
    }
    *frame = std::move(mFrames.front().bytes);
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
