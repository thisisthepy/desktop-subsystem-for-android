// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "composer.h"

#include "log.h"

namespace dsa::hwc {

Composer::Composer(std::shared_ptr<TaskComposer> tasks, const DisplayConfig& config)
    : mTasks(std::move(tasks)), mConfig(config) {}

ndk::ScopedAStatus Composer::createClient(std::shared_ptr<composer3::IComposerClient>* out) {
    std::lock_guard<std::mutex> lock(mMutex);
    if (!mClient.expired()) {
        // One SurfaceFlinger, one client; a second would fight it for the display.
        return ndk::ScopedAStatus::fromServiceSpecificError(composer3::IComposer::EX_NO_RESOURCES);
    }
    auto client = ndk::SharedRefBase::make<ComposerClient>(mTasks, mConfig);
    std::weak_ptr<ComposerClient> weak = client;
    mTasks->setRefreshRequest([weak] {
        if (auto c = weak.lock()) {
            c->requestRefresh();
        }
    });
    mClient = client;
    makeBinderRealtime(client->asBinder().get());
    *out = client;
    HWC_LOGI("SurfaceFlinger connected: one %dx%d display at %d dpi", mConfig.width, mConfig.height,
             mConfig.dpi);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Composer::getCapabilities(std::vector<composer3::Capability>* caps) {
    // No present fences are handed back: frames are finished when presentDisplay returns.
    *caps = {composer3::Capability::PRESENT_FENCE_IS_NOT_RELIABLE};
    return ndk::ScopedAStatus::ok();
}

}  // namespace dsa::hwc
