// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "host_stream.h"

#include <sys/socket.h>
// After sys/socket.h, which defines the struct sockaddr it uses.
#include <linux/vm_sockets.h>
#include <unistd.h>

#include <cerrno>
#include <cstdlib>
#include <cstring>

#include "log.h"

namespace dsa::hwc {

HostStream::HostStream(size_t bufferSize, uint32_t port)
    : IOStream(bufferSize), mPort(port), mBufferSize(bufferSize) {}

HostStream::~HostStream() {
    if (mFd >= 0) {
        close(mFd);
    }
    free(mBuffer);
}

bool HostStream::open() {
    const int fd = socket(AF_VSOCK, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) {
        HWC_LOGE("cannot create a vsock socket for the host renderer: %s", strerror(errno));
        return false;
    }
    sockaddr_vm address{};
    address.svm_family = AF_VSOCK;
    address.svm_port = mPort;
    address.svm_cid = VMADDR_CID_HOST;
    if (::connect(fd, reinterpret_cast<sockaddr*>(&address), sizeof(address)) < 0) {
        HWC_LOGW("cannot reach the host renderer on vsock port %u: %s", mPort, strerror(errno));
        close(fd);
        return false;
    }
    mFd = fd;
    mFailed = false;
    return true;
}

void* HostStream::allocBuffer(size_t minSize) {
    const size_t size = minSize > mBufferSize ? minSize : mBufferSize;
    auto* grown = static_cast<unsigned char*>(realloc(mBuffer, size));
    if (!grown) {
        HWC_LOGE("cannot allocate %zu bytes for the host renderer stream", size);
        mFailed = true;
        return nullptr;
    }
    mBuffer = grown;
    mBufferSize = size;
    return mBuffer;
}

int HostStream::commitBuffer(size_t size) { return size == 0 ? 0 : writeFully(mBuffer, size); }

int HostStream::writeFully(const void* buf, size_t len) {
    if (mFd < 0 || mFailed || (!buf && len != 0)) {
        mFailed = true;
        return -1;
    }
    const auto* cursor = static_cast<const unsigned char*>(buf);
    while (len != 0) {
        const ssize_t written = send(mFd, cursor, len, MSG_NOSIGNAL);
        if (written > 0) {
            cursor += written;
            len -= static_cast<size_t>(written);
        } else if (written < 0 && errno == EINTR) {
            continue;
        } else {
            HWC_LOGE("the host renderer connection broke while writing: %s", strerror(errno));
            mFailed = true;
            return -1;
        }
    }
    return 0;
}

const unsigned char* HostStream::readFully(void* buf, size_t len) {
    if (mFd < 0 || mFailed || (!buf && len != 0)) {
        mFailed = true;
        if (buf && len) {
            memset(buf, 0, len);
        }
        return nullptr;
    }
    auto* cursor = static_cast<unsigned char*>(buf);
    size_t left = len;
    while (left != 0) {
        const ssize_t received = recv(mFd, cursor, left, 0);
        if (received > 0) {
            cursor += received;
            left -= static_cast<size_t>(received);
        } else if (received < 0 && errno == EINTR) {
            continue;
        } else {
            HWC_LOGE("the host renderer connection broke while reading: %s",
                     received == 0 ? "closed by the host" : strerror(errno));
            mFailed = true;
            // The encoder returns whatever is in the buffer; make it a defined value.
            memset(buf, 0, len);
            return nullptr;
        }
    }
    return static_cast<const unsigned char*>(buf);
}

const unsigned char* HostStream::commitBufferAndReadFully(size_t size, void* buf, size_t len) {
    if (commitBuffer(size) != 0) {
        if (buf && len) {
            memset(buf, 0, len);
        }
        return nullptr;
    }
    return readFully(buf, len);
}

const unsigned char* HostStream::read(void* buf, size_t* inoutLen) {
    if (mFd < 0 || mFailed || !buf || !inoutLen) {
        return nullptr;
    }
    for (;;) {
        const ssize_t received = recv(mFd, buf, *inoutLen, 0);
        if (received > 0) {
            *inoutLen = static_cast<size_t>(received);
            return static_cast<const unsigned char*>(buf);
        }
        if (received < 0 && errno == EINTR) {
            continue;
        }
        mFailed = true;
        return nullptr;
    }
}

}  // namespace dsa::hwc
