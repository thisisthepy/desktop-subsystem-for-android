// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What the guest's copy of each host-visible allocation holds, as the host renderer last
// exchanged it, so that a read back sends only what the device changed since.
//
// A Hyper-V VM has no memory the guest and the host share. The guest driver keeps a copy of
// host-visible memory, sends what the app wrote at each submit, and reads the device's writes
// back whenever the app learns that the device finished (a fence, a semaphore, a wait). The
// app's mapped memory adds up to tens of megabytes, while a frame's work writes into a few
// kilobytes of it or none, so sending all of it back at every fence made every frame wait for
// tens of megabytes to cross the socket: an app drew at 5 to 10 frames a second, and a resized
// window waited a third of a second or more for its first frame at the new size.
//
// So the host keeps, for each 4 KiB block of an allocation, the hash of what the guest's copy
// holds: what the guest sent at its last flush, or what the host sent at its last read back.
// A range asked to be read back goes in full when any of its blocks changed since, and as
// nothing (size 0) when none did. A guest reads exactly the size it is sent and leaves its
// copy as it is for a range of size 0, so the wire format is the one the guest already reads,
// and an older guest that asks for whole allocations gets them only when they changed.
//
// A block the host has no hash for (never exchanged, or exchanged only in part) counts as
// changed, so the first read back of an allocation sends all of it. The hash is the one the
// guest uses for its own blocks (dsa_shadow_blocks.h). Each record is 16 bytes per 4 KiB.

#pragma once

#include <cstdint>
#include <mutex>
#include <unordered_map>
#include <vector>

#include "dsa_shadow_blocks.h"

namespace dsa {

class ShadowRecord {
   public:
    // The guest sent `size` bytes (`data`) for `offset` of `memory`, whose host mapping holds
    // `mappedSize` bytes. Each block the range covers from its start is known to hold them;
    // a block it covers only from part way along is no longer known.
    void flushed(uint64_t memory, uint64_t mappedSize, uint64_t offset, const uint8_t* data,
                 uint64_t size) {
        if (size == 0 || offset >= mappedSize) {
            return;
        }
        std::lock_guard<std::mutex> lock(mutex_);
        Blocks& blocks = blocksOf(memory, mappedSize);
        const uint64_t end = clampEnd(offset, size, mappedSize);
        for (uint64_t block = offset / kShadowBlockSize; block * kShadowBlockSize < end; block++) {
            const uint64_t start = block * kShadowBlockSize;
            if (start < offset) {
                blocks[block] = Block{};
                continue;
            }
            const uint64_t length = blockEnd(start, end) - start;
            blocks[block] = Block{shadowBlockHash(data + (start - offset), length), length};
        }
    }

    // Whether the `size` bytes at `offset` of `memory`, whose host mapping (`mapped`) holds
    // `mappedSize` bytes, must go to the guest: true when any block of the range changed
    // since the guest last had it, or is not known. When true, the blocks are recorded as
    // the guest is about to hold them. Their hashes are taken before the bytes are sent, so
    // a block the device writes meanwhile is sent again at the next read back.
    bool mustSend(uint64_t memory, const uint8_t* mapped, uint64_t mappedSize, uint64_t offset,
                  uint64_t size) {
        if (size == 0 || offset >= mappedSize) {
            // The decoder sends nothing for a range past the mapping's end.
            return false;
        }
        std::lock_guard<std::mutex> lock(mutex_);
        Blocks& blocks = blocksOf(memory, mappedSize);
        const uint64_t end = clampEnd(offset, size, mappedSize);
        const uint64_t first = offset / kShadowBlockSize;
        bool changed = false;
        scratch_.clear();
        for (uint64_t block = first; block * kShadowBlockSize < end; block++) {
            const uint64_t start = block * kShadowBlockSize;
            if (start < offset) {
                // Part of a block: the guest's copy of the rest of it is not known.
                changed = true;
                scratch_.push_back(Block{});
                continue;
            }
            const uint64_t length = blockEnd(start, end) - start;
            const Block now{shadowBlockHash(mapped + start, length), length};
            if (!blocks[block].same(now)) {
                changed = true;
            }
            scratch_.push_back(now);
        }
        if (changed) {
            for (size_t i = 0; i < scratch_.size(); i++) {
                blocks[first + i] = scratch_[i];
            }
        }
        return changed;
    }

    // `memory` was freed: a later allocation with the same handle starts unknown.
    void forget(uint64_t memory) {
        std::lock_guard<std::mutex> lock(mutex_);
        records_.erase(memory);
    }

    size_t tracked() const {
        std::lock_guard<std::mutex> lock(mutex_);
        return records_.size();
    }

   private:
    struct Block {
        uint64_t hash = 0;
        // How many bytes the hash covers; 0 when the block is not known.
        uint64_t length = 0;

        bool same(const Block& other) const {
            return length != 0 && length == other.length && hash == other.hash;
        }
    };
    using Blocks = std::vector<Block>;

    Blocks& blocksOf(uint64_t memory, uint64_t mappedSize) {
        Blocks& blocks = records_[memory];
        const size_t count = static_cast<size_t>((mappedSize + kShadowBlockSize - 1) / kShadowBlockSize);
        if (blocks.size() != count) {
            // A new allocation, or one the handle now names with another size.
            blocks.assign(count, Block{});
        }
        return blocks;
    }

    static uint64_t clampEnd(uint64_t offset, uint64_t size, uint64_t mappedSize) {
        return size > mappedSize - offset ? mappedSize : offset + size;
    }

    static uint64_t blockEnd(uint64_t start, uint64_t end) {
        return end - start < kShadowBlockSize ? end : start + kShadowBlockSize;
    }

    mutable std::mutex mutex_;
    std::unordered_map<uint64_t, Blocks> records_;
    std::vector<Block> scratch_;
};

// The renderer's one record, shared by every guest stream's decoder.
inline ShadowRecord& shadowRecord() {
    static ShadowRecord* record = new ShadowRecord();
    return *record;
}

}  // namespace dsa
