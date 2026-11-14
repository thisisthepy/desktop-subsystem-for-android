// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What the guest's Vulkan driver sends to the host before a submit: only the blocks of
// host-visible memory that changed.

#include <cstdio>
#include <cstdlib>
#include <vector>

#include "dsa_shadow_blocks.h"

namespace {

using dsa::kShadowBlockSize;
using dsa::ShadowBlocks;

int failures = 0;

void check(bool ok, const char* test, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "%s: %s\n", test, what);
        failures++;
    }
}

void fr5_memory_nothing_wrote_to_sends_nothing() {
    const char* test = __func__;
    std::vector<uint8_t> memory(64 * kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    check(blocks.changed(memory.data()).empty(), test, "a new allocation has nothing to send");
}

void fr5_a_write_sends_only_its_block() {
    const char* test = __func__;
    std::vector<uint8_t> memory(64 * kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    memory[5 * kShadowBlockSize + 17] = 1;
    const auto ranges = blocks.changed(memory.data());
    check(ranges.size() == 1, test, "one range");
    check(ranges[0].offset == 5 * kShadowBlockSize, test, "at the block written");
    check(ranges[0].size == kShadowBlockSize, test, "one block long");
    check(blocks.changed(memory.data()).empty(), test, "and it is sent only once");
}

void fr5_a_change_to_any_byte_of_a_block_is_seen() {
    const char* test = __func__;
    std::vector<uint8_t> memory(kShadowBlockSize, 0x5a);
    ShadowBlocks blocks(memory.size());
    blocks.changed(memory.data());
    for (uint64_t at = 0; at < kShadowBlockSize; at++) {
        memory[at] ^= 0x01;
        if (blocks.changed(memory.data()).size() != 1) {
            std::fprintf(stderr, "%s: a change at byte %llu was missed\n", test,
                         static_cast<unsigned long long>(at));
            failures++;
        }
    }
}

void fr5_neighbouring_blocks_go_as_one_range() {
    const char* test = __func__;
    std::vector<uint8_t> memory(8 * kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    memory[1 * kShadowBlockSize] = 1;
    memory[2 * kShadowBlockSize] = 1;
    memory[6 * kShadowBlockSize] = 1;
    const auto ranges = blocks.changed(memory.data());
    check(ranges.size() == 2, test, "two ranges");
    check(ranges[0].offset == kShadowBlockSize && ranges[0].size == 2 * kShadowBlockSize, test,
          "the neighbours merge");
    check(ranges[1].offset == 6 * kShadowBlockSize && ranges[1].size == kShadowBlockSize, test,
          "the other stays apart");
}

void fr5_the_last_range_ends_at_the_end_of_the_memory() {
    const char* test = __func__;
    const uint64_t size = 2 * kShadowBlockSize + 100;
    std::vector<uint8_t> memory(size, 0);
    ShadowBlocks blocks(size);
    check(blocks.changed(memory.data()).empty(), test, "a short last block starts clean");
    memory[size - 1] = 1;
    const auto ranges = blocks.changed(memory.data());
    check(ranges.size() == 1, test, "one range");
    check(ranges[0].offset == 2 * kShadowBlockSize && ranges[0].size == 100, test,
          "no further than the memory's end");
}

void fr5_writing_zeros_over_data_is_a_change() {
    const char* test = __func__;
    std::vector<uint8_t> memory(kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    memory[9] = 7;
    blocks.changed(memory.data());
    memory[9] = 0;
    check(blocks.changed(memory.data()).size() == 1, test, "the host must get the zeros");
}

void fr5_what_the_host_sent_back_is_not_sent_to_it_again() {
    const char* test = __func__;
    std::vector<uint8_t> memory(4 * kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    memory[kShadowBlockSize] = 3;
    blocks.refresh(memory.data());
    check(blocks.changed(memory.data()).empty(), test, "the host has these bytes already");
}

// An allocation whose last block ends 9 to 31 bytes past a 32-byte step: the hash read those
// bytes into an 8-byte word, which overran it (SurfaceFlinger aborted on it at boot), and a
// change past the eighth of them went unseen.
void fr5_a_change_to_the_last_bytes_of_an_odd_sized_allocation_is_seen() {
    const char* test = __func__;
    for (size_t extra = 1; extra < 32; extra++) {
        const size_t size = 2 * kShadowBlockSize + 32 + extra;
        for (size_t at = size - extra; at < size; at++) {
            std::vector<uint8_t> memory(size, 0);
            ShadowBlocks blocks(memory.size());
            check(blocks.changed(memory.data()).empty(), test, "nothing written yet");
            memory[at] = 0x5a;
            const auto ranges = blocks.changed(memory.data());
            check(ranges.size() == 1 && ranges[0].offset == 2 * kShadowBlockSize &&
                      ranges[0].size == 32 + extra,
                  test, "a byte of the tail changed, and its block is sent");
        }
    }
}

// A read back the host sends whole (an older renderer, or a piece with one changed block):
// only the blocks the device changed come in, and what the app wrote since its last submit
// stays and still goes at the next flush.
void fr4_a_read_back_takes_only_what_the_device_changed_and_keeps_the_apps_writes() {
    const char* test = __func__;
    std::vector<uint8_t> memory(4 * kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    std::vector<uint8_t> host(memory.size(), 0);
    host[2 * kShadowBlockSize + 5] = 9;  // the device wrote block 2
    memory[0 * kShadowBlockSize + 3] = 4;  // the app wrote block 0, not flushed yet
    const uint64_t taken = blocks.receive(memory.data(), 0, host.data(), host.size());
    check(taken == kShadowBlockSize, test, "one block came in");
    check(memory[2 * kShadowBlockSize + 5] == 9, test, "the device's write is in the copy");
    check(memory[3] == 4, test, "the app's write stays");
    const auto ranges = blocks.changed(memory.data());
    check(ranges.size() == 1 && ranges[0].offset == 0 && ranges[0].size == kShadowBlockSize,
          test, "and only the app's write goes at the next flush");
}

void fr4_a_read_back_of_unchanged_memory_takes_nothing() {
    const char* test = __func__;
    std::vector<uint8_t> memory(8 * kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    memory[100] = 1;
    blocks.changed(memory.data());
    const std::vector<uint8_t> host = memory;
    check(blocks.receive(memory.data(), 0, host.data(), host.size()) == 0, test,
          "the host has what the copy last sent");
    check(blocks.changed(memory.data()).empty(), test, "nothing to send either");
}

void fr4_a_block_read_back_in_part_goes_whole_at_the_next_flush() {
    const char* test = __func__;
    std::vector<uint8_t> memory(2 * kShadowBlockSize, 0);
    ShadowBlocks blocks(memory.size());
    const std::vector<uint8_t> part(100, 7);
    check(blocks.receive(memory.data(), kShadowBlockSize + 10, part.data(), part.size()) == 100,
          test, "the bytes sent come in");
    check(memory[kShadowBlockSize + 10] == 7 && memory[kShadowBlockSize + 109] == 7, test,
          "where they were sent for");
    const auto ranges = blocks.changed(memory.data());
    check(ranges.size() == 1 && ranges[0].offset == kShadowBlockSize &&
              ranges[0].size == kShadowBlockSize,
          test, "the block is not known to match the host's, so it goes whole");
}

void fr4_a_read_back_past_the_end_is_cut_at_the_end() {
    const char* test = __func__;
    const uint64_t size = kShadowBlockSize + 100;
    std::vector<uint8_t> memory(size, 0);
    ShadowBlocks blocks(size);
    const std::vector<uint8_t> host(2 * kShadowBlockSize, 3);
    check(blocks.receive(memory.data(), 0, host.data(), host.size()) == size, test,
          "no further than the copy's end");
    check(memory[size - 1] == 3, test, "the short last block came in");
    check(blocks.changed(memory.data()).empty(), test, "and is known to match");
}

}  // namespace

int main() {
    fr4_a_read_back_takes_only_what_the_device_changed_and_keeps_the_apps_writes();
    fr4_a_read_back_of_unchanged_memory_takes_nothing();
    fr4_a_block_read_back_in_part_goes_whole_at_the_next_flush();
    fr4_a_read_back_past_the_end_is_cut_at_the_end();
    fr5_a_change_to_the_last_bytes_of_an_odd_sized_allocation_is_seen();
    fr5_memory_nothing_wrote_to_sends_nothing();
    fr5_a_write_sends_only_its_block();
    fr5_a_change_to_any_byte_of_a_block_is_seen();
    fr5_neighbouring_blocks_go_as_one_range();
    fr5_the_last_range_ends_at_the_end_of_the_memory();
    fr5_writing_zeros_over_data_is_a_change();
    fr5_what_the_host_sent_back_is_not_sent_to_it_again();
    if (failures != 0) {
        std::fprintf(stderr, "%d failed\n", failures);
        return EXIT_FAILURE;
    }
    std::puts("shadow blocks: all passed");
    return EXIT_SUCCESS;
}
