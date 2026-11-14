// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What the host renderer sends back when the guest reads host-visible memory back: only the
// ranges the device changed since the guest last had them (dsa_shadow_record.h).

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <vector>

#include "dsa_shadow_record.h"

namespace {

using dsa::kShadowBlockSize;
using dsa::ShadowRecord;

int failures = 0;

void check(bool ok, const char* test, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "%s: %s\n", test, what);
        failures++;
    }
}

constexpr uint64_t kMemory = 0x1234;

void fr4_memory_never_exchanged_is_sent_whole() {
    const char* test = __func__;
    ShadowRecord record;
    std::vector<uint8_t> host(16 * kShadowBlockSize, 0);
    check(record.mustSend(kMemory, host.data(), host.size(), 0, host.size()), test,
          "the guest's copy is not known");
    check(!record.mustSend(kMemory, host.data(), host.size(), 0, host.size()), test,
          "and once sent, it is");
}

void fr4_what_the_guest_flushed_is_not_sent_back() {
    const char* test = __func__;
    ShadowRecord record;
    std::vector<uint8_t> host(16 * kShadowBlockSize, 0);
    record.mustSend(kMemory, host.data(), host.size(), 0, host.size());
    std::vector<uint8_t> written(2 * kShadowBlockSize, 0x42);
    // As the decoder does: the guest's bytes go into the host's memory and are recorded.
    std::copy(written.begin(), written.end(), host.begin() + 4 * kShadowBlockSize);
    record.flushed(kMemory, host.size(), 4 * kShadowBlockSize, written.data(), written.size());
    check(!record.mustSend(kMemory, host.data(), host.size(), 0, host.size()), test,
          "the guest has those bytes already");
}

void fr4_a_device_write_sends_only_its_range_once() {
    const char* test = __func__;
    ShadowRecord record;
    std::vector<uint8_t> host(32 * kShadowBlockSize, 0);
    const uint64_t piece = 16 * kShadowBlockSize;
    record.mustSend(kMemory, host.data(), host.size(), 0, piece);
    record.mustSend(kMemory, host.data(), host.size(), piece, piece);
    host[piece + 7] = 1;  // the device wrote into the second piece
    check(!record.mustSend(kMemory, host.data(), host.size(), 0, piece), test,
          "the first piece did not change");
    check(record.mustSend(kMemory, host.data(), host.size(), piece, piece), test,
          "the second did");
    check(!record.mustSend(kMemory, host.data(), host.size(), piece, piece), test,
          "and goes only once");
}

void fr4_a_range_that_starts_inside_a_block_is_sent() {
    const char* test = __func__;
    ShadowRecord record;
    std::vector<uint8_t> host(4 * kShadowBlockSize, 0);
    record.mustSend(kMemory, host.data(), host.size(), 0, host.size());
    check(record.mustSend(kMemory, host.data(), host.size(), 100, kShadowBlockSize), test,
          "the guest's copy of the rest of that block is not known");
}

void fr4_a_flush_that_starts_inside_a_block_leaves_it_unknown() {
    const char* test = __func__;
    ShadowRecord record;
    std::vector<uint8_t> host(4 * kShadowBlockSize, 0);
    record.mustSend(kMemory, host.data(), host.size(), 0, host.size());
    const std::vector<uint8_t> written(10, 5);
    record.flushed(kMemory, host.size(), kShadowBlockSize + 3, written.data(), written.size());
    check(record.mustSend(kMemory, host.data(), host.size(), kShadowBlockSize, kShadowBlockSize),
          test, "the block is sent again");
}

void fr4_a_short_last_block_is_compared_at_its_length() {
    const char* test = __func__;
    ShadowRecord record;
    const uint64_t size = 3 * kShadowBlockSize + 100;
    std::vector<uint8_t> host(size, 0);
    const std::vector<uint8_t> tail(100, 8);
    std::copy(tail.begin(), tail.end(), host.begin() + 3 * kShadowBlockSize);
    record.flushed(kMemory, size, 3 * kShadowBlockSize, tail.data(), tail.size());
    check(!record.mustSend(kMemory, host.data(), size, 3 * kShadowBlockSize, 100), test,
          "the tail the guest flushed is not sent back");
}

void fr4_a_freed_handle_starts_unknown() {
    const char* test = __func__;
    ShadowRecord record;
    std::vector<uint8_t> host(4 * kShadowBlockSize, 0);
    record.mustSend(kMemory, host.data(), host.size(), 0, host.size());
    record.forget(kMemory);
    check(record.tracked() == 0, test, "nothing is kept for freed memory");
    check(record.mustSend(kMemory, host.data(), host.size(), 0, host.size()), test,
          "a new allocation with the same handle is sent whole");
}

void fr4_a_range_past_the_mapping_sends_nothing() {
    const char* test = __func__;
    ShadowRecord record;
    std::vector<uint8_t> host(kShadowBlockSize, 0);
    check(!record.mustSend(kMemory, host.data(), host.size(), host.size(), 10), test,
          "the decoder answers it with size 0");
}

}  // namespace

int main() {
    fr4_memory_never_exchanged_is_sent_whole();
    fr4_what_the_guest_flushed_is_not_sent_back();
    fr4_a_device_write_sends_only_its_range_once();
    fr4_a_range_that_starts_inside_a_block_is_sent();
    fr4_a_flush_that_starts_inside_a_block_leaves_it_unknown();
    fr4_a_short_last_block_is_compared_at_its_length();
    fr4_a_freed_handle_starts_unknown();
    fr4_a_range_past_the_mapping_sends_nothing();
    if (failures != 0) {
        std::fprintf(stderr, "%d failed\n", failures);
        return EXIT_FAILURE;
    }
    std::puts("shadow record: all passed");
    return EXIT_SUCCESS;
}
