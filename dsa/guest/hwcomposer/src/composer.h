// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// IComposer, the service SurfaceFlinger finds: it hands out the one client.

#pragma once

#include <memory>
#include <mutex>

#include <aidl/android/hardware/graphics/composer3/BnComposer.h>

#include "composer_client.h"
#include "display.h"
#include "task_composer.h"

namespace dsa::hwc {

class Composer : public composer3::BnComposer {
   public:
    Composer(std::shared_ptr<TaskComposer> tasks, const DisplayConfig& config);

    ndk::ScopedAStatus createClient(std::shared_ptr<composer3::IComposerClient>* client) override;
    ndk::ScopedAStatus getCapabilities(std::vector<composer3::Capability>* caps) override;

    // The host asked for a larger display (DisplaySize): SurfaceFlinger's client grows it
    // now, and a client made later, for a SurfaceFlinger that restarted, starts at that size.
    void growDisplay(int64_t width, int64_t height);

   private:
    std::shared_ptr<TaskComposer> mTasks;
    DisplayConfig mConfig;
    std::mutex mMutex;
    std::weak_ptr<ComposerClient> mClient;
};

}  // namespace dsa::hwc
