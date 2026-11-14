// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "bounded_thread.h"

#include <utility>

namespace dsa::codec2 {

BoundedThread::~BoundedThread() { stop(std::chrono::milliseconds(0)); }

void BoundedThread::start(std::function<void()> body) {
    if (mThread.joinable() || !body) {
        return;
    }
    mDone = std::make_shared<Done>();
    mThread = std::thread([done = mDone, body = std::move(body)] {
        body();
        std::lock_guard<std::mutex> lock(done->lock);
        done->done = true;
        done->changed.notify_all();
    });
}

bool BoundedThread::stop(std::chrono::milliseconds wait) {
    if (!mThread.joinable()) {
        return true;
    }
    if (mThread.get_id() == std::this_thread::get_id()) {
        mThread.detach();
        return true;
    }
    bool ended = false;
    {
        std::unique_lock<std::mutex> lock(mDone->lock);
        ended = mDone->changed.wait_for(lock, wait, [this] { return mDone->done; });
    }
    if (ended) {
        mThread.join();
    } else {
        // It finishes on its own, holding what it uses.
        mThread.detach();
    }
    mDone.reset();
    return ended;
}

}  // namespace dsa::codec2
