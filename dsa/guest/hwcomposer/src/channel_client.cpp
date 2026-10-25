// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "channel_client.h"

#include <android/binder_manager.h>

#include <chrono>
#include <string>

#include <aidl/io/github/thisisthepy/dsa/channel/BnChannelListener.h>

#include "log.h"

namespace dsa::hwc {

namespace channel = aidl::io::github::thisisthepy::dsa::channel;

namespace {

class Listener : public channel::BnChannelListener {
   public:
    explicit Listener(ChannelClient::FrameHandler onFrame) : mOnFrame(std::move(onFrame)) {}

    ndk::ScopedAStatus onFrame(const std::vector<uint8_t>& frame) override {
        mOnFrame(frame);
        return ndk::ScopedAStatus::ok();
    }

   private:
    ChannelClient::FrameHandler mOnFrame;
};

std::string serviceName() { return std::string(channel::IChannel::descriptor) + "/default"; }

}  // namespace

ChannelClient::ChannelClient(FrameHandler onFrame, ConnectHandler onConnected)
    : mOnFrame(std::move(onFrame)),
      mOnConnected(std::move(onConnected)),
      mDeath(AIBinder_DeathRecipient_new(&ChannelClient::onBinderDied)) {}

ChannelClient::~ChannelClient() {
    {
        std::lock_guard<std::mutex> lock(mMutex);
        mStopping = true;
    }
    mWake.notify_all();
    if (mThread.joinable()) {
        mThread.join();
    }
}

void ChannelClient::start() { mThread = std::thread([this] { run(); }); }

void ChannelClient::onBinderDied(void* cookie) {
    auto* self = static_cast<ChannelClient*>(cookie);
    HWC_LOGW("the control channel daemon died; waiting for it to come back");
    {
        std::lock_guard<std::mutex> lock(self->mMutex);
        self->mChannel.reset();
        self->mLost = true;
    }
    self->mWake.notify_all();
}

bool ChannelClient::send(const std::vector<uint8_t>& frame) {
    std::shared_ptr<channel::IChannel> channel;
    {
        std::lock_guard<std::mutex> lock(mMutex);
        channel = mChannel;
    }
    if (!channel) {
        HWC_LOGW("the control channel daemon is not there; a frame of %zu bytes waits for it",
                 frame.size());
        return false;
    }
    const ndk::ScopedAStatus status = channel->send(frame);
    if (!status.isOk()) {
        HWC_LOGE("the control channel daemon refused a frame: %s", status.getDescription().c_str());
        return false;
    }
    return true;
}

void ChannelClient::run() {
    const std::string name = serviceName();
    bool warned = false;
    std::unique_lock<std::mutex> lock(mMutex);
    while (!mStopping) {
        if (!mLost) {
            mWake.wait(lock, [this] { return mStopping || mLost; });
            continue;
        }
        lock.unlock();

        ndk::SpAIBinder binder(AServiceManager_checkService(name.c_str()));
        std::shared_ptr<channel::IChannel> found;
        if (binder.get() != nullptr) {
            found = channel::IChannel::fromBinder(binder);
        }
        bool subscribed = false;
        if (found) {
            auto listener = ndk::SharedRefBase::make<Listener>(mOnFrame);
            const ndk::ScopedAStatus status = found->subscribe(listener);
            if (status.isOk()) {
                AIBinder_linkToDeath(binder.get(), mDeath.get(), this);
                subscribed = true;
            } else {
                HWC_LOGE("the control channel daemon refused the subscription: %s",
                         status.getDescription().c_str());
            }
        } else if (!warned) {
            HWC_LOGI("waiting for %s", name.c_str());
            warned = true;
        }

        lock.lock();
        if (subscribed) {
            mChannel = found;
            mLost = false;
            warned = false;
            lock.unlock();
            HWC_LOGI("subscribed to the control channel");
            mOnConnected();
            lock.lock();
        } else {
            mWake.wait_for(lock, std::chrono::seconds(1), [this] { return mStopping; });
        }
    }
}

}  // namespace dsa::hwc
