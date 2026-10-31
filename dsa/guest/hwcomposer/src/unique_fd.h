// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#pragma once

#include <poll.h>
#include <unistd.h>

#include <cerrno>
#include <utility>

namespace dsa::hwc {

// An owned file descriptor, closed when it goes away.
class UniqueFd {
   public:
    UniqueFd() = default;
    explicit UniqueFd(int fd) : mFd(fd) {}
    ~UniqueFd() { reset(); }
    UniqueFd(UniqueFd&& other) noexcept : mFd(other.release()) {}
    UniqueFd& operator=(UniqueFd&& other) noexcept {
        if (this != &other) {
            reset(other.release());
        }
        return *this;
    }
    UniqueFd(const UniqueFd&) = delete;
    UniqueFd& operator=(const UniqueFd&) = delete;

    int get() const { return mFd; }
    bool ok() const { return mFd >= 0; }
    int release() { return std::exchange(mFd, -1); }
    void reset(int fd = -1) {
        if (mFd >= 0) {
            close(mFd);
        }
        mFd = fd;
    }

   private:
    int mFd = -1;
};

// Waits for a sync fence to signal: a sync file becomes readable when it does. Returns
// false if it has not within `timeoutMs`.
inline bool waitForFence(const UniqueFd& fence, int timeoutMs) {
    if (!fence.ok()) {
        return true;
    }
    pollfd pfd{fence.get(), POLLIN, 0};
    for (;;) {
        const int ready = poll(&pfd, 1, timeoutMs);
        if (ready > 0) {
            return (pfd.revents & (POLLERR | POLLNVAL)) == 0;
        }
        if (ready < 0 && (errno == EINTR || errno == EAGAIN)) {
            continue;
        }
        return false;
    }
}

}  // namespace dsa::hwc
