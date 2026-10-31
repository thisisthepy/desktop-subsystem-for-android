/*
 * Copyright 2019 Google
 * SPDX-License-Identifier: MIT
 */
#pragma once

#include "aemu/base/AlignedBuf.h"
#include "aemu/base/Allocator.h"

#include <vector>
#include <unordered_set>
#include <cstdlib>
#include <cstdint>
#include <inttypes.h>

namespace gfxstream {
namespace aemu {

using Allocator = gfxstream::guest::Allocator;

class BumpPool : public Allocator {
public:
    BumpPool(size_t startingBytes = 4096) : mStorage(startingBytes / sizeof(uint64_t)) { }
    ~BumpPool() override {
        freeAll();
    }

    void* alloc(size_t wantedSize) override {
        size_t wantedSizeRoundedUp =
            sizeof(uint64_t) * ((wantedSize + sizeof(uint64_t) - 1) / (sizeof(uint64_t)));

        mTotalWantedThisGeneration += wantedSizeRoundedUp;
        if (mAllocPos + wantedSizeRoundedUp > mStorage.size() * sizeof(uint64_t)) {
            mNeedRealloc = true;
            void* fallbackPtr = malloc(wantedSizeRoundedUp);
            mFallbackPtrs.insert(fallbackPtr);
            return fallbackPtr;
        }
        void* allocPtr = (void*)(((unsigned char*)mStorage.data()) + mAllocPos);
        mAllocPos += wantedSizeRoundedUp;
        return allocPtr;
    }

    void freeAll() {
        mAllocPos = 0;
        if (mNeedRealloc) {
            mStorage.resize((mTotalWantedThisGeneration * 2) / sizeof(uint64_t));
            mNeedRealloc = false;
            for (auto ptr : mFallbackPtrs) {
                free(ptr);
            }
            mFallbackPtrs.clear();
        }
        mTotalWantedThisGeneration = 0;
    }
private:
    gfxstream::guest::AlignedBuf<uint64_t, 8> mStorage;
    std::unordered_set<void*> mFallbackPtrs;
    size_t mAllocPos = 0;
    size_t mTotalWantedThisGeneration = 0;
    bool mNeedRealloc = false;
};

} // namespace aemu

namespace guest {
using BumpPool = gfxstream::aemu::BumpPool;
} // namespace guest

} // namespace gfxstream

namespace android {
namespace base {
using Allocator = gfxstream::guest::Allocator;
using BumpPool = gfxstream::aemu::BumpPool;
} // namespace base
} // namespace android

using Allocator = gfxstream::guest::Allocator;
using BumpPool = gfxstream::aemu::BumpPool;
