// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// When a render thread's Vulkan reply buffer goes back to its normal size
// (dsa_reply_buffer.h, patch 0028), run on the build machine against a buffer that grows the
// way VulkanStream's write buffer does.

#include "dsa_reply_buffer.h"

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

using namespace std::chrono_literals;

constexpr size_t kMiB = 1024 * 1024;

// The write side of VulkanStream after patch 0028: replies are buffered, the buffer grows to
// twice what it must hold, and each commit asks whether it goes back to its normal size.
struct ReplyStream {
    std::vector<uint8_t> buffer;
    size_t pos = 0;
    dsa::ReplyClock::time_point lastLarge{};
    bool seenLarge = false;
    int reallocations = 0;

    void write(size_t size) {
        if (size > buffer.size() - pos) {
            buffer.resize((pos + size) << 1);
            ++reallocations;
        }
        std::memset(buffer.data() + pos, 0x5a, size);
        pos += size;
    }

    void commit(dsa::ReplyClock::time_point now) {
        const size_t sent = pos;
        pos = 0;
        if (dsa::replyBufferShrinks(sent, buffer.capacity(), now, lastLarge, seenLarge)) {
            dsa::shrinkReplyBuffer(buffer);
            ++reallocations;
        }
    }

    void reply(size_t size, dsa::ReplyClock::time_point now) {
        write(size);
        commit(now);
    }
};

// nfr1: a large readback (a 32 MiB copy of host-visible memory) and then a quiet while: the
// next small reply gives the buffer back to its normal size.
static void nfr1_a_large_reply_then_idle_gives_the_buffer_back() {
    const auto start = dsa::ReplyClock::now();
    ReplyStream stream;
    stream.reply(4 * 1024, start);
    stream.reply(32 * kMiB, start + 10ms);
    EXPECT(stream.buffer.capacity() >= 64 * kMiB);
    stream.reply(4 * 1024, start + 10ms + dsa::kReplyQuietFor);
    EXPECT(stream.buffer.capacity() == dsa::kReplyKeepBytes);
    EXPECT(stream.buffer.size() == dsa::kReplyKeepBytes);
    // The buffer still works at its normal size, and grows again when it must.
    stream.reply(200 * 1024, start + 3s);
    EXPECT(stream.buffer.capacity() == dsa::kReplyKeepBytes);
    stream.reply(8 * kMiB, start + 4s);
    EXPECT(stream.buffer.capacity() >= 16 * kMiB);
}

// nfr1: a small reply soon after a large one keeps the buffer: the next large one would only
// make it again.
static void nfr1_a_small_reply_soon_after_a_large_one_keeps_the_buffer() {
    const auto start = dsa::ReplyClock::now();
    ReplyStream stream;
    stream.reply(32 * kMiB, start);
    const size_t grown = stream.buffer.capacity();
    stream.reply(4 * 1024, start + dsa::kReplyQuietFor - 1ms);
    EXPECT(stream.buffer.capacity() == grown);
}

// nfr1: a video's steady frames, each with a large readback and small replies between, never
// reallocate once the buffer has grown.
static void nfr1_steady_frames_do_not_reallocate() {
    const auto start = dsa::ReplyClock::now();
    ReplyStream stream;
    stream.reply(32 * kMiB, start);
    const int afterFirst = stream.reallocations;
    const uint8_t* data = stream.buffer.data();
    for (int frame = 1; frame <= 600; ++frame) {
        const auto at = start + frame * 16ms;
        stream.reply(4 * 1024, at);
        stream.reply(32 * kMiB, at + 1ms);
        stream.reply(512, at + 2ms);
    }
    EXPECT(stream.reallocations == afterFirst);
    EXPECT(stream.buffer.data() == data);
}

// nfr1: replies that never went past the normal size, and commits that sent nothing, change
// nothing.
static void nfr1_small_replies_and_empty_commits_change_nothing() {
    const auto start = dsa::ReplyClock::now();
    ReplyStream stream;
    for (int i = 0; i < 100; ++i) {
        stream.reply(64 * 1024, start + i * 16ms);
    }
    EXPECT(stream.reallocations == 1);
    // A read commits whatever is buffered, often nothing: it is not a reply.
    stream.reply(32 * kMiB, start + 2s);
    const size_t grown = stream.buffer.capacity();
    stream.commit(start + 10s);
    EXPECT(stream.buffer.capacity() == grown);
}

int main() {
    nfr1_a_large_reply_then_idle_gives_the_buffer_back();
    nfr1_a_small_reply_soon_after_a_large_one_keeps_the_buffer();
    nfr1_steady_frames_do_not_reallocate();
    nfr1_small_replies_and_empty_commits_change_nothing();
    if (failures) {
        std::fprintf(stderr, "%d failures\n", failures);
        return 1;
    }
    std::puts("reply buffer: all passed");
    return 0;
}
