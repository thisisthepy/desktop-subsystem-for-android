// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "output_gate.h"

#include <poll.h>

#include <algorithm>
#include <cerrno>
#include <thread>

namespace dsa::codec2 {

WaitResult waitReadable(int fd, std::chrono::steady_clock::time_point deadline, const OutputGate& gate,
                        uint64_t ticket, std::chrono::milliseconds slice) {
    using std::chrono::duration_cast;
    using std::chrono::milliseconds;
    while (true) {
        if (!gate.open(ticket)) {
            return WaitResult::Closed;
        }
        const auto now = std::chrono::steady_clock::now();
        if (now >= deadline) {
            return WaitResult::TimedOut;
        }
        // Rounded up, so a deadline less than a millisecond away is not polled with 0 in a
        // tight loop.
        const auto left = duration_cast<milliseconds>(deadline - now) + milliseconds(1);
        const milliseconds wait = std::min(left, slice);
        if (fd < 0) {
            std::this_thread::sleep_for(wait);
            continue;
        }
        pollfd p{fd, POLLIN, 0};
        const int ready = poll(&p, 1, static_cast<int>(wait.count()));
        if (ready < 0) {
            if (errno == EINTR) {
                continue;
            }
            return WaitResult::Failed;
        }
        if (ready == 0) {
            continue;
        }
        if ((p.revents & POLLIN) != 0) {
            return WaitResult::Ready;
        }
        return WaitResult::Failed;
    }
}

}  // namespace dsa::codec2
