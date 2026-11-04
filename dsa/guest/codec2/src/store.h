// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// DSA's Codec 2.0 component store (IComponentStore/default): the decoders whose format the
// host decodes on its GPU. The host says which formats when the store asks; until it has
// said, the store lists none, so the framework uses the guest's software decoders.

#pragma once

#include <chrono>
#include <condition_variable>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include <aidl/android/hardware/media/c2/BnComponentStore.h>
#include <util/C2InterfaceHelper.h>

#include "video_messages.h"

namespace dsa::codec2 {

namespace c2aidl = ::aidl::android::hardware::media::c2;

class Store : public c2aidl::BnComponentStore {
   public:
    Store();
    ~Store() override;

    // Starts asking the host what it decodes, until it answers.
    void start();

    ndk::ScopedAStatus copyBuffer(const c2aidl::Buffer& src, const c2aidl::Buffer& dst) override;
    ndk::ScopedAStatus createComponent(const std::string& name,
                                       const std::shared_ptr<c2aidl::IComponentListener>& listener,
                                       const std::shared_ptr<::aidl::android::hardware::media::bufferpool2::IClientManager>& pool,
                                       std::shared_ptr<c2aidl::IComponent>* component) override;
    ndk::ScopedAStatus createInterface(const std::string& name,
                                       std::shared_ptr<c2aidl::IComponentInterface>* intf) override;
    ndk::ScopedAStatus getConfigurable(std::shared_ptr<c2aidl::IConfigurable>* configurable) override;
    ndk::ScopedAStatus getPoolClientManager(
        std::shared_ptr<::aidl::android::hardware::media::bufferpool2::IClientManager>* manager) override;
    ndk::ScopedAStatus getStructDescriptors(const std::vector<int32_t>& indices,
                                            std::vector<c2aidl::StructDescriptor>* descriptors) override;
    ndk::ScopedAStatus listComponents(std::vector<c2aidl::IComponentStore::ComponentTraits>* traits) override;
    ndk::ScopedAStatus createInputSurface(std::shared_ptr<c2aidl::IInputSurface>* surface) override;

   private:
    // The host's support for `name`'s format, waiting a while for the host to say.
    std::optional<CodecSupport> supportFor(const std::string& name);
    std::optional<std::vector<CodecSupport>> capabilities(std::chrono::milliseconds wait);
    void ask();

    std::shared_ptr<C2ReflectorHelper> mReflector;
    std::shared_ptr<c2aidl::IConfigurable> mConfigurable;
    std::mutex mLock;
    std::condition_variable mAnswered;
    std::optional<std::vector<CodecSupport>> mCaps;
    bool mStopping = false;
    std::thread mAsker;
};

}  // namespace dsa::codec2
