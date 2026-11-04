// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// dsa-power's IPower (android.hardware.power, version 5). Android's power manager sets its
// power modes and asks for boosts here; each one the host acts on goes to the host as a
// PowerHint, and the host runs the guest's processors to match. Hint sessions (ADPF) are not
// offered: the host cannot steer single guest threads, only the guest's processors as a
// whole, which the modes and boosts already do.

#pragma once

#include <functional>
#include <memory>
#include <mutex>
#include <vector>

#include <aidl/android/hardware/power/BnPower.h>

#include "power_modes.h"

namespace dsa::power {

class Power : public aidl::android::hardware::power::BnPower {
   public:
    using Sender = std::function<bool(const std::vector<uint8_t>&)>;

    explicit Power(Sender send) : mSend(std::move(send)) {}

    // The channel subscribed again: the host is told every mode that is on.
    void channelConnected();

    ndk::ScopedAStatus setMode(aidl::android::hardware::power::Mode type, bool enabled) override;
    ndk::ScopedAStatus isModeSupported(aidl::android::hardware::power::Mode type, bool* supported) override;
    ndk::ScopedAStatus setBoost(aidl::android::hardware::power::Boost type, int32_t durationMs) override;
    ndk::ScopedAStatus isBoostSupported(aidl::android::hardware::power::Boost type, bool* supported) override;
    ndk::ScopedAStatus createHintSession(
        int32_t tgid, int32_t uid, const std::vector<int32_t>& threadIds, int64_t durationNanos,
        std::shared_ptr<aidl::android::hardware::power::IPowerHintSession>* session) override;
    ndk::ScopedAStatus getHintSessionPreferredRate(int64_t* nanos) override;
    ndk::ScopedAStatus createHintSessionWithConfig(
        int32_t tgid, int32_t uid, const std::vector<int32_t>& threadIds, int64_t durationNanos,
        aidl::android::hardware::power::SessionTag tag, aidl::android::hardware::power::SessionConfig* config,
        std::shared_ptr<aidl::android::hardware::power::IPowerHintSession>* session) override;
    ndk::ScopedAStatus getSessionChannel(int32_t tgid, int32_t uid,
                                         aidl::android::hardware::power::ChannelConfig* channel) override;
    ndk::ScopedAStatus closeSessionChannel(int32_t tgid, int32_t uid) override;

   private:
    Sender mSend;
    // Held while a mode changes and is sent, so the host hears a mode's changes in order.
    std::mutex mOrder;
    ModeSet mModes;
};

}  // namespace dsa::power
