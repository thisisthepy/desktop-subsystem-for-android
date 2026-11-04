// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "host_link.h"

#include <linux/vm_sockets.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include <cerrno>

#include "log.h"

namespace dsa::codec2 {

namespace {

// The largest frame the host may send: a 4096 by 2304 picture with its framing, and room.
constexpr uint32_t kMaxHostFrame = 16 * 1024 * 1024;

bool writeAll(int fd, const std::vector<uint8_t>& bytes) {
    size_t done = 0;
    while (done < bytes.size()) {
        const ssize_t n = send(fd, bytes.data() + done, bytes.size() - done, MSG_NOSIGNAL);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) return false;
        done += static_cast<size_t>(n);
    }
    return true;
}

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

bool readFrame(int fd, std::vector<uint8_t>* frame) {
    uint8_t header[4];
    if (!readAll(fd, header, sizeof(header))) return false;
    const uint32_t size = static_cast<uint32_t>(header[0]) | (static_cast<uint32_t>(header[1]) << 8) |
                          (static_cast<uint32_t>(header[2]) << 16) | (static_cast<uint32_t>(header[3]) << 24);
    if (size > kMaxHostFrame) return false;
    frame->resize(size);
    return readAll(fd, frame->data(), size);
}

void setTimeout(int fd, int seconds) {
    timeval timeout{seconds, 0};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
}

// Connects to the video port and does the handshake; the host's capabilities come next.
// Returns the socket, or -1 with the reason in `error`.
int connectAndGreet(std::string* error, Capabilities* caps) {
    const int fd = socket(AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) {
        *error = "cannot make a vsock socket";
        return -1;
    }
    sockaddr_vm address{};
    address.svm_family = AF_VSOCK;
    address.svm_cid = VMADDR_CID_HOST;
    address.svm_port = kVideoPort;
    if (connect(fd, reinterpret_cast<const sockaddr*>(&address), sizeof(address)) != 0) {
        *error = "the host's video port is not open (is the DSA window manager running?)";
        ::close(fd);
        return -1;
    }
    setTimeout(fd, 5);
    std::vector<uint8_t> frame;
    if (!writeAll(fd, withLength(encodeHello("dsa-codec2"))) || !readFrame(fd, &frame)) {
        *error = "the host did not answer on the video port";
        ::close(fd);
        return -1;
    }
    const HostMessage hello = decodeHostFrame(frame.data(), frame.size());
    if (const auto* rejected = std::get_if<Rejected>(&hello)) {
        *error = "the host refused the video port: " + rejected->detail;
        ::close(fd);
        return -1;
    }
    if (!std::holds_alternative<Accepted>(hello) || !readFrame(fd, &frame)) {
        *error = "the host answered the video port's hello with something else";
        ::close(fd);
        return -1;
    }
    const HostMessage offered = decodeHostFrame(frame.data(), frame.size());
    const auto* got = std::get_if<Capabilities>(&offered);
    if (got == nullptr) {
        *error = "the host did not say what it decodes";
        ::close(fd);
        return -1;
    }
    *caps = *got;
    return fd;
}

}  // namespace

std::optional<std::vector<CodecSupport>> queryCapabilities() {
    std::string error;
    Capabilities caps;
    const int fd = connectAndGreet(&error, &caps);
    if (fd < 0) {
        CODEC_LOGW("%s", error.c_str());
        return std::nullopt;
    }
    ::close(fd);
    return caps.codecs;
}

std::unique_ptr<HostDecoder> HostDecoder::open(Codec codec, uint32_t width, uint32_t height,
                                               MessageHandler onMessage, std::string* error) {
    Capabilities caps;
    const int fd = connectAndGreet(error, &caps);
    if (fd < 0) {
        return nullptr;
    }
    std::vector<uint8_t> frame;
    if (!writeAll(fd, withLength(encodeOpen(codec, width, height))) || !readFrame(fd, &frame)) {
        *error = "the host did not answer the request for a decoder";
        ::close(fd);
        return nullptr;
    }
    const HostMessage answer = decodeHostFrame(frame.data(), frame.size());
    const auto* opened = std::get_if<Opened>(&answer);
    if (opened == nullptr || !opened->ok) {
        *error = opened ? opened->error : "the host answered the request for a decoder with something else";
        ::close(fd);
        return nullptr;
    }
    // Pictures come when the decoder has them; the reader waits as long as it takes.
    timeval none{0, 0};
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &none, sizeof(none));
    std::unique_ptr<HostDecoder> decoder(new HostDecoder(fd, std::move(onMessage)));
    decoder->mReader = std::thread([raw = decoder.get()] { raw->read(); });
    return decoder;
}

HostDecoder::HostDecoder(int fd, MessageHandler onMessage) : mFd(fd), mOnMessage(std::move(onMessage)) {}

HostDecoder::~HostDecoder() { close(); }

bool HostDecoder::send(const std::vector<uint8_t>& frame) {
    std::lock_guard<std::mutex> lock(mSendLock);
    if (mClosed.load()) {
        return false;
    }
    return writeAll(mFd, withLength(frame));
}

void HostDecoder::read() {
    std::vector<uint8_t> frame;
    while (!mClosed.load() && readFrame(mFd, &frame)) {
        const HostMessage message = decodeHostFrame(frame.data(), frame.size());
        if (!mClosed.load()) {
            mOnMessage(message);
        }
    }
    if (!mClosed.load()) {
        CODEC_LOGW("the host's decoder went away");
        mOnMessage(DecodeError{"the host's decoder went away"});
    }
}

void HostDecoder::close() {
    if (mClosed.exchange(true)) {
        return;
    }
    shutdown(mFd, SHUT_RDWR);
    if (mReader.joinable()) {
        if (mReader.get_id() == std::this_thread::get_id()) {
            mReader.detach();
        } else {
            mReader.join();
        }
    }
    std::lock_guard<std::mutex> lock(mSendLock);
    ::close(mFd);
    mFd = -1;
}

}  // namespace dsa::codec2
