// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What the guest Vulkan driver does when its stream to the host renderer ends
// (dsa_lost_stream.h), run on the build machine.

#include "dsa_lost_stream.h"

#include <chrono>
#include <cstdint>
#include <cstdio>

using namespace std::chrono_literals;

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

static const std::chrono::steady_clock::time_point kStart{};

static void nfr5_a_stream_that_ended_is_lost_once_and_the_process_hears_of_it() {
    dsa::StreamGenerations::beginNew();
    dsa::LostStream stream;
    EXPECT(!stream.lost());
    EXPECT(!dsa::StreamGenerations::anyLost());
    // The first failure is logged; the ones after it are not.
    EXPECT(stream.markLost());
    EXPECT(!stream.markLost());
    EXPECT(stream.lost());
    EXPECT(dsa::StreamGenerations::anyLost());
}

static void nfr5_a_lost_stream_reads_zeros_and_writes_nowhere() {
    // Zeros are no handles and no counts: the encoder reads no arrays after them, and the
    // caller sees VK_ERROR_DEVICE_LOST instead of the result it would have read.
    uint8_t reply[16];
    for (auto& byte : reply) byte = 0xAB;
    dsa::LostStream::zero(reply, sizeof(reply));
    bool allZero = true;
    for (auto byte : reply) allZero = allZero && byte == 0;
    EXPECT(allZero);
    dsa::LostStream::zero(nullptr, 0);

    dsa::LostStream stream;
    uint8_t* small = stream.scratch(8);
    EXPECT(small != nullptr);
    uint8_t* large = stream.scratch(1 << 20);
    EXPECT(large != nullptr);
    large[(1 << 20) - 1] = 1;  // the whole packet fits
    EXPECT(stream.scratch(4) == large);
}

static void nfr5_a_lost_thread_connects_again_only_after_the_process_starts_over() {
    dsa::StreamGenerations::beginNew();
    dsa::LostStream stream;
    stream.markLost();
    // Until an instance-level call starts a new generation, the lost stream stays: calls on
    // the old device go on reporting VK_ERROR_DEVICE_LOST, and nothing reaches a renderer
    // that knows nothing of them.
    EXPECT(!dsa::shouldReconnect(stream.lost(), stream.generation(), kStart + 1s, kStart));
    dsa::StreamGenerations::beginNew();
    EXPECT(!dsa::StreamGenerations::anyLost());
    EXPECT(dsa::shouldReconnect(stream.lost(), stream.generation(), kStart + 1s, kStart));
    // A stream from the new generation, and one that never ended, stay.
    dsa::LostStream fresh;
    EXPECT(!dsa::shouldReconnect(fresh.lost(), fresh.generation(), kStart + 1s, kStart));
    fresh.markLost();
    EXPECT(!dsa::shouldReconnect(fresh.lost(), fresh.generation(), kStart + 1s, kStart));
}

static void nfr5_attempts_to_connect_again_are_spaced_while_the_renderer_restarts() {
    dsa::StreamGenerations::beginNew();
    dsa::LostStream stream;
    stream.markLost();
    dsa::StreamGenerations::beginNew();
    const auto attempt = kStart + 10s;
    EXPECT(!dsa::shouldReconnect(true, stream.generation(), attempt + 100ms, attempt));
    EXPECT(dsa::shouldReconnect(true, stream.generation(), attempt + dsa::kReconnectInterval,
                                attempt));
}

int main() {
    nfr5_a_stream_that_ended_is_lost_once_and_the_process_hears_of_it();
    nfr5_a_lost_stream_reads_zeros_and_writes_nowhere();
    nfr5_a_lost_thread_connects_again_only_after_the_process_starts_over();
    nfr5_attempts_to_connect_again_are_spaced_while_the_renderer_restarts();
    if (failures) {
        std::fprintf(stderr, "%d expectations failed\n", failures);
        return 1;
    }
    std::printf("lost stream: ok\n");
    return 0;
}
