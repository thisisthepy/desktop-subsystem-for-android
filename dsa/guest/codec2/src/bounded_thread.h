// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// A thread that a stop waits for only so long.
//
// The component writes the host's pictures into the app's buffers on a thread of its own,
// and a picture there can be inside a binder call to the app (a buffer of its surface, a
// finished work) when the app calls stop() or release(). Those calls come on the app's
// binder threads, and the app may be waiting for them on its UI thread: a stop that waited
// for the picture's call waited as long as the app took to answer it, which, when the app's
// surface was being replaced (a window moved to another display), held its UI thread past
// Android's input timeout. A stop now waits a short while, then lets the thread finish its
// picture on its own: everything it then does is dropped by the output gate.

#pragma once

#include <chrono>
#include <condition_variable>
#include <functional>
#include <memory>
#include <mutex>
#include <thread>

namespace dsa::codec2 {

class BoundedThread {
   public:
    BoundedThread() = default;
    ~BoundedThread();
    BoundedThread(const BoundedThread&) = delete;
    BoundedThread& operator=(const BoundedThread&) = delete;

    // Runs `body` on a thread of its own. Nothing when it is already running or `body` is
    // empty.
    void start(std::function<void()> body);

    // Waits at most `wait` for the thread to end, and lets it go either way. True when it
    // ended in time (or none was running). Called on the thread itself, it does not wait.
    bool stop(std::chrono::milliseconds wait);

    // Whether a thread was started and not yet stopped.
    bool started() const { return mThread.joinable(); }

   private:
    struct Done {
        std::mutex lock;
        std::condition_variable changed;
        bool done = false;
    };

    std::shared_ptr<Done> mDone;
    std::thread mThread;
};

}  // namespace dsa::codec2
