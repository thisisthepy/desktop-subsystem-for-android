// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "output_buffer.h"

#include <algorithm>

namespace dsa::codec2 {

AllocateFailure classifyAllocateError(int32_t error) {
    switch (error) {
        case kC2Blocking:
        case kC2TimedOut:
        case kC2Canceled:
            return AllocateFailure::Wait;
        case kC2BadState:
            return AllocateFailure::Stopped;
        default:
            return AllocateFailure::Failed;
    }
}

bool peerIsGone(int32_t exceptionCode, int32_t status) {
    return status == kStatusDeadObject && (exceptionCode == kExTransactionFailed || exceptionCode == 0);
}

PictureSize bufferSizeFor(uint32_t width, uint32_t height) {
    return PictureSize{width + (width & 1), height + (height & 1)};
}

Crop cropFor(uint32_t width, uint32_t height, const PictureSize& buffer) {
    return Crop{0, 0, std::min(width, buffer.width), std::min(height, buffer.height)};
}

bool FailureThrottle::shouldLog(uint32_t* count) {
    ++mCount;
    *count = mCount;
    return mCount == 1 || (mEvery > 0 && mCount % mEvery == 0);
}

}  // namespace dsa::codec2
