// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// dsa-health: the guest's health HAL. The guest's battery is the host computer's, as the
// host reports it over the control channel.

#include <android/binder_manager.h>
#include <android/binder_process.h>

#include <csignal>
#include <memory>
#include <string>

#include "channel_client.h"
#include "health_service.h"
#include "log.h"
#include "power_messages.h"

using dsa::health::Health;
using dsa::power::ChannelClient;

int main() {
    signal(SIGPIPE, SIG_IGN);

    auto health = ndk::SharedRefBase::make<Health>();
    ChannelClient channel(
        [health](const std::vector<uint8_t>& frame) {
            if (const auto host = dsa::power::decodeHostPower(frame.data(), frame.size())) {
                health->hostPower(*host);
            }
        },
        [] {});

    ABinderProcess_setThreadPoolMaxThreadCount(2);
    ABinderProcess_startThreadPool();

    const std::string instance = std::string(Health::descriptor) + "/default";
    const binder_status_t status = AServiceManager_addService(health->asBinder().get(), instance.c_str());
    if (status != STATUS_OK) {
        POWER_LOGE("cannot register %s with the service manager (status %d)", instance.c_str(), status);
        return 1;
    }
    POWER_LOGI("serving %s; the battery is the host's", instance.c_str());

    channel.start();
    ABinderProcess_joinThreadPool();
    return 1;
}
