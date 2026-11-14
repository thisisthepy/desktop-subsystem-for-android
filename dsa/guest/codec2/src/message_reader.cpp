// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "message_reader.h"

#include <sys/socket.h>

#include <cerrno>
#include <utility>

namespace dsa::codec2 {

namespace {

bool readAll(int fd, uint8_t* out, size_t size) {
    size_t done = 0;
    while (done < size) {
        const ssize_t n = recv(fd, out + done, size - done, 0);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return false;
        done += static_cast<size_t>(n);
    }
    return true;
}

}  // namespace

bool readHostFrame(int fd, std::vector<uint8_t>* frame) {
    uint8_t header[4];
    if (!readAll(fd, header, sizeof(header))) return false;
    const uint32_t size = static_cast<uint32_t>(header[0]) | (static_cast<uint32_t>(header[1]) << 8) |
                          (static_cast<uint32_t>(header[2]) << 16) | (static_cast<uint32_t>(header[3]) << 24);
    if (size > kMaxHostFrame) return false;
    frame->resize(size);
    return readAll(fd, frame->data(), size);
}

MessageReader::MessageReader(int fd, Handler handler) : mState(std::make_shared<State>()) {
    mState->fd = fd;
    mState->handler = std::move(handler);
    mThread = std::thread([state = mState] { run(state); });
}

MessageReader::~MessageReader() { stop(); }

void MessageReader::run(const std::shared_ptr<State>& state) {
    // The handler of the moment, or nothing once stopped. A copy is called, so a stop (or the
    // owner's end) while it runs changes nothing it uses.
    const auto current = [&state]() -> Handler {
        std::lock_guard<std::mutex> lock(state->lock);
        if (state->stopped.load()) {
            return nullptr;
        }
        return state->handler;
    };
    std::vector<uint8_t> frame;
    while (!state->stopped.load() && readHostFrame(state->fd, &frame)) {
        HostMessage message = decodeHostFrame(frame.data(), frame.size());
        if (Handler handler = current()) {
            handler(std::move(message));
        }
    }
    if (Handler handler = current()) {
        handler(DecodeError{"the host's decoder went away"});
    }
}

void MessageReader::silence() {
    // The handler goes outside the lock: dropping it may drop the last reference to whatever
    // it holds.
    Handler dropped;
    std::lock_guard<std::mutex> lock(mState->lock);
    mState->stopped.store(true);
    dropped.swap(mState->handler);
}

void MessageReader::stop() {
    silence();
    if (!mThread.joinable()) {
        return;
    }
    if (mThread.get_id() == std::this_thread::get_id()) {
        // Inside the handler: the thread ends once it returns, holding its own state.
        mThread.detach();
    } else {
        mThread.join();
    }
}

}  // namespace dsa::codec2
