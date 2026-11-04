// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "power_service.h"

#include "log.h"
#include "power_messages.h"

namespace dsa::power {

using aidl::android::hardware::power::Boost;
using aidl::android::hardware::power::ChannelConfig;
using aidl::android::hardware::power::IPowerHintSession;
using aidl::android::hardware::power::Mode;
using aidl::android::hardware::power::SessionConfig;
using aidl::android::hardware::power::SessionTag;

namespace {

ndk::ScopedAStatus unsupported() { return ndk::ScopedAStatus::fromExceptionCode(EX_UNSUPPORTED_OPERATION); }

}  // namespace

void Power::channelConnected() {
    std::lock_guard<std::mutex> order(mOrder);
    const std::vector<int32_t> on = mModes.on();
    for (int32_t mode : on) {
        mSend(encodeModeHint(mode, true));
    }
    POWER_LOGI("subscribed to the control channel; told the host the %zu power modes on", on.size());
}

ndk::ScopedAStatus Power::setMode(Mode type, bool enabled) {
    const auto mode = static_cast<int32_t>(type);
    if (!modeSupported(mode)) {
        return ndk::ScopedAStatus::ok();
    }
    // The framework sets a mode again when nothing changed; the host hears only changes.
    std::lock_guard<std::mutex> order(mOrder);
    if (mModes.set(mode, enabled)) {
        mSend(encodeModeHint(mode, enabled));
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Power::isModeSupported(Mode type, bool* supported) {
    *supported = modeSupported(static_cast<int32_t>(type));
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Power::setBoost(Boost type, int32_t durationMs) {
    const auto boost = static_cast<int32_t>(type);
    if (boostSupported(boost)) {
        mSend(encodeBoostHint(boost, durationMs));
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Power::isBoostSupported(Boost type, bool* supported) {
    *supported = boostSupported(static_cast<int32_t>(type));
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Power::createHintSession(int32_t, int32_t, const std::vector<int32_t>&, int64_t,
                                            std::shared_ptr<IPowerHintSession>* session) {
    *session = nullptr;
    return unsupported();
}

ndk::ScopedAStatus Power::getHintSessionPreferredRate(int64_t* nanos) {
    *nanos = -1;
    return unsupported();
}

ndk::ScopedAStatus Power::createHintSessionWithConfig(int32_t, int32_t, const std::vector<int32_t>&, int64_t,
                                                      SessionTag, SessionConfig*,
                                                      std::shared_ptr<IPowerHintSession>* session) {
    *session = nullptr;
    return unsupported();
}

ndk::ScopedAStatus Power::getSessionChannel(int32_t, int32_t, ChannelConfig*) { return unsupported(); }

ndk::ScopedAStatus Power::closeSessionChannel(int32_t, int32_t) { return ndk::ScopedAStatus::ok(); }

}  // namespace dsa::power
