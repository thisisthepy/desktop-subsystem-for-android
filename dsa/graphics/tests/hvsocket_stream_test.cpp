// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The guest's Hyper-V socket stream, built from the patched gfxstream tree and run on the
// build machine over a socket pair in place of AF_VSOCK.

#include <sys/socket.h>
#include <unistd.h>

#include <cstdarg>
#include <cstdio>
#include <cstring>

// The stream connects over AF_VSOCK, which a build machine may not have; the test hands it
// one end of a socket pair instead.
#define private public
#include "HvSocketStream.h"
#undef private

// The stream logs through liblog, which is not on the build machine.
extern "C" int __android_log_print(int, const char*, const char*, ...) { return 0; }

static int failures = 0;

#define EXPECT(cond)                                                     \
    do {                                                                 \
        if (!(cond)) {                                                   \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                  \
        }                                                                \
    } while (0)

static void pr3_reading_nothing_succeeds() {
    // A reply can carry an empty array (no descriptor pool IDs, no data). The Vulkan
    // encoder reads it back with a length of zero and aborts the app when the stream
    // returns null, as it did on the machine: "FATAL: Could not read back 0 bytes".
    int pair[2];
    EXPECT(socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == 0);
    HvSocketStream stream(64);
    stream.fd_ = pair[0];

    unsigned char buffer[4] = {};
    EXPECT(stream.readFully(buffer, 0) != nullptr);
    EXPECT(stream.readFully(nullptr, 0) != nullptr);
    EXPECT(stream.readback(buffer, 0) != nullptr);

    // And the stream is still in step afterwards.
    EXPECT(write(pair[1], "abcd", 4) == 4);
    EXPECT(stream.readFully(buffer, 4) == buffer);
    EXPECT(std::memcmp(buffer, "abcd", 4) == 0);
    close(pair[1]);
}

static void pr3_a_closed_connection_is_a_failed_read() {
    int pair[2];
    EXPECT(socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == 0);
    HvSocketStream stream(64);
    stream.fd_ = pair[0];
    close(pair[1]);

    unsigned char buffer[4] = {};
    EXPECT(stream.readFully(buffer, 4) == nullptr);
}

int main() {
    pr3_reading_nothing_succeeds();
    pr3_a_closed_connection_is_a_failed_read();
    std::printf("%s\n", failures == 0 ? "ok" : "FAILED");
    return failures == 0 ? 0 : 1;
}
