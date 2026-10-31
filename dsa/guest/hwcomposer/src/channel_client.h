// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The hwcomposer's end of the guest control channel: dsa-channeld's binder service. It
// subscribes for the agent's task messages and sends TaskFrames. The daemon can start
// after the hwcomposer and can restart, so the client waits for it and subscribes again
// whenever it comes back.

#pragma once

#include <android/binder_auto_utils.h>

#include <condition_variable>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>

#include <aidl/io/github/thisisthepy/dsa/channel/IChannel.h>

namespace dsa::hwc {

class ChannelClient {
   public:
    using FrameHandler = std::function<void(const std::vector<uint8_t>&)>;
    using ConnectHandler = std::function<void()>;

    // `onFrame` gets every frame the daemon forwards; `onConnected` runs after each
    // successful subscription.
    ChannelClient(FrameHandler onFrame, ConnectHandler onConnected);
    ~ChannelClient();

    void start();
    // Sends one encoded Envelope. Returns false, with the reason logged, when the daemon is
    // not there; a pairing that misses it is announced again on the next subscription.
    bool send(const std::vector<uint8_t>& frame);

   private:
    void run();
    static void onBinderDied(void* cookie);

    FrameHandler mOnFrame;
    ConnectHandler mOnConnected;

    std::mutex mMutex;
    std::condition_variable mWake;
    std::shared_ptr<aidl::io::github::thisisthepy::dsa::channel::IChannel> mChannel;
    bool mLost = true;
    bool mStopping = false;
    ndk::ScopedAIBinder_DeathRecipient mDeath;
    std::thread mThread;
};

}  // namespace dsa::hwc
