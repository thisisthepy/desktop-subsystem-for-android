// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// libbase's unique_fd, for the platform's Codec 2.0 headers (see logging.h).

#pragma once

#include <unistd.h>

namespace android {
namespace base {

class unique_fd {
   public:
    unique_fd() = default;
    explicit unique_fd(int fd) : mFd(fd) {}
    unique_fd(unique_fd&& other) noexcept : mFd(other.release()) {}
    unique_fd& operator=(unique_fd&& other) noexcept {
        reset(other.release());
        return *this;
    }
    unique_fd(const unique_fd&) = delete;
    unique_fd& operator=(const unique_fd&) = delete;
    ~unique_fd() { reset(); }

    int get() const { return mFd; }
    bool ok() const { return mFd >= 0; }
    int release() {
        const int fd = mFd;
        mFd = -1;
        return fd;
    }
    void reset(int fd = -1) {
        if (mFd >= 0) {
            ::close(mFd);
        }
        mFd = fd;
    }

   private:
    int mFd = -1;
};

}  // namespace base
}  // namespace android
