// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "cpu_upload_policy.h"

#include "dsa_shared_color_buffer.h"

namespace dsa::hwc {

bool hostComposes(uint32_t drmFourcc) { return dsa_shared_color_buffer_yuv_layout(drmFourcc) == 0; }

bool LayerPictures::show(int64_t layer, uint32_t buffer) {
    auto [it, added] = mShown.emplace(layer, buffer);
    if (added) {
        return true;
    }
    const bool changed = it->second != buffer;
    it->second = buffer;
    return changed;
}

bool UploadFailureLog::shouldLog(uint32_t* count) {
    ++mCount;
    *count = mCount;
    return mCount == 1 || (mEvery > 0 && mCount % mEvery == 0);
}

}  // namespace dsa::hwc
