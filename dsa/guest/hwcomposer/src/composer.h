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

   private:
    std::shared_ptr<TaskComposer> mTasks;
    DisplayConfig mConfig;
    std::mutex mMutex;
    std::weak_ptr<ComposerClient> mClient;
};

}  // namespace dsa::hwc
