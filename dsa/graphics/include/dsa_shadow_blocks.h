// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Which parts of a guest's copy of host-visible memory changed since the host last had them.
//
// A Hyper-V VM has no memory the guest and the host share, so the guest driver keeps a copy of
// host-visible memory and the stream carries it to the host before each submit. An app's
// allocations add up to tens of megabytes, and a frame changes a few kilobytes of them.
// Sending all of it for every submit makes every frame wait for tens of megabytes to cross a
// Hyper-V socket, so only the blocks that changed are sent.
//
// Nothing tells the driver when an app writes through a mapped pointer, so each block's hash
// is kept and compared at each flush. Hashing is an order of magnitude faster than sending,
// and keeps 8 bytes per 4 KiB block rather than a second copy of the memory.
//
// A new allocation counts as all zero, which is what the driver's copy starts as, so blocks
// the app never wrote are never sent. The host's memory is undefined there until written.

#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

namespace dsa {

inline constexpr uint64_t kShadowBlockSize = 4096;

// One block's hash. A change confined to one 8-byte word always changes it: each step is a
// bijection of the lane and of the word.
inline uint64_t shadowBlockHash(const uint8_t* data, size_t size) {
    constexpr uint64_t kPrime = 0x9E3779B97F4A7C15ULL;
    uint64_t lanes[4] = {0x243F6A8885A308D3ULL, 0x13198A2E03707344ULL, 0xA4093822299F31D0ULL,
                         0x082EFA98EC4E6C89ULL};
    size_t at = 0;
    uint64_t words[4];
    for (; size - at >= sizeof(words); at += sizeof(words)) {
        std::memcpy(words, data + at, sizeof(words));
        for (int lane = 0; lane < 4; lane++) {
            lanes[lane] = (lanes[lane] ^ words[lane]) * kPrime;
        }
    }
    uint64_t tail = 0;
    if (at != size) {
        std::memcpy(&tail, data + at, size - at);
    }
    uint64_t hash = (lanes[0] ^ tail) * kPrime;
    for (int lane = 1; lane < 4; lane++) {
        hash = (hash ^ lanes[lane]) * kPrime;
    }
    return hash ^ (hash >> 29);
}

class ShadowBlocks {
   public:
    // A byte range of the memory.
    struct Range {
        uint64_t offset;
        uint64_t size;
    };

    explicit ShadowBlocks(uint64_t size)
        : size_(size), hashes_(blockCount(size), zeroHash(kShadowBlockSize)) {
        if (size % kShadowBlockSize != 0) {
            hashes_.back() = zeroHash(size % kShadowBlockSize);
        }
    }

    // The ranges of `memory` that changed since the last call, each as large as it can be, in
    // order. Each comes back once: the caller is taken to send them.
    std::vector<Range> changed(const uint8_t* memory) {
        std::vector<Range> ranges;
        for (size_t block = 0; block < hashes_.size(); block++) {
            const uint64_t offset = block * kShadowBlockSize;
            const uint64_t length = blockSize(offset);
            const uint64_t hash = shadowBlockHash(memory + offset, length);
            if (hash == hashes_[block]) {
                continue;
            }
            hashes_[block] = hash;
            if (!ranges.empty() && ranges.back().offset + ranges.back().size == offset) {
                ranges.back().size += length;
            } else {
                ranges.push_back(Range{offset, length});
            }
        }
        return ranges;
    }

    // The host wrote `memory` (a read back), so it and the host agree on all of it now.
    void refresh(const uint8_t* memory) {
        for (size_t block = 0; block < hashes_.size(); block++) {
            const uint64_t offset = block * kShadowBlockSize;
            hashes_[block] = shadowBlockHash(memory + offset, blockSize(offset));
        }
    }

    uint64_t size() const { return size_; }

   private:
    static size_t blockCount(uint64_t size) {
        return static_cast<size_t>((size + kShadowBlockSize - 1) / kShadowBlockSize);
    }

    static uint64_t zeroHash(uint64_t size) {
        const std::vector<uint8_t> zeros(static_cast<size_t>(size), 0);
        return shadowBlockHash(zeros.data(), zeros.size());
    }

    uint64_t blockSize(uint64_t offset) const {
        return size_ - offset < kShadowBlockSize ? size_ - offset : kShadowBlockSize;
    }

    uint64_t size_;
    std::vector<uint64_t> hashes_;
};

}  // namespace dsa
