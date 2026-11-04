// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// dsa-power: the guest's power HAL. The framework's power modes and boosts go to the host,
// which runs the guest's processors to match.

#include <android/binder_manager.h>
#include <android/binder_process.h>

#include <csignal>
#include <memory>
#include <string>

#include "channel_client.h"
#include "log.h"
#include "power_service.h"

using namespace dsa::power;

int main() {
    signal(SIGPIPE, SIG_IGN);

    std::shared_ptr<Power> power;
    ChannelClient channel([](const std::vector<uint8_t>&) {},
                          [&power] {
                              if (power) {
                                  power->channelConnected();
                              }
                          });
    power = ndk::SharedRefBase::make<Power>(
        [&channel](const std::vector<uint8_t>& frame) { return channel.send(frame); });

    ABinderProcess_setThreadPoolMaxThreadCount(2);
    ABinderProcess_startThreadPool();

    const std::string instance = std::string(Power::descriptor) + "/default";
    const binder_status_t status = AServiceManager_addService(power->asBinder().get(), instance.c_str());
    if (status != STATUS_OK) {
        POWER_LOGE("cannot register %s with the service manager (status %d)", instance.c_str(), status);
        return 1;
    }
    POWER_LOGI("serving %s; the host runs the guest's processors as its modes and boosts ask", instance.c_str());

    channel.start();
    ABinderProcess_joinThreadPool();
    return 1;
}
