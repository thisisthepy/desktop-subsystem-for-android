// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// dsa-hwcomposer: the guest's composer HAL. SurfaceFlinger composes one freeform display
// through it; it sends the host each task's frames instead, one host display per task.

#include <android/binder_manager.h>
#include <android/binder_process.h>

#include <csignal>
#include <memory>
#include <string>
#include <variant>

#include "channel_client.h"
#include "channel_messages.h"
#include "composer.h"
#include "display.h"
#include "log.h"
#include "task_composer.h"

using namespace dsa::hwc;

int main() {
    // A host connection that drops must not kill the process.
    signal(SIGPIPE, SIG_IGN);

    const DisplayConfig config = DisplayConfig::fromProperties();
    auto tasks = std::make_shared<TaskComposer>();

    ChannelClient channel(
        [tasks](const std::vector<uint8_t>& frame) {
            const ChannelEvent event = decodeChannelFrame(frame.data(), frame.size());
            if (const auto* update = std::get_if<TaskUpdate>(&event)) {
                tasks->taskUpdated(*update);
            } else if (const auto* end = std::get_if<TaskEnd>(&event)) {
                tasks->taskEnded(end->id);
            }
        },
        [tasks] { tasks->channelConnected(); });
    tasks->setSender([&channel](const std::vector<uint8_t>& frame) { channel.send(frame); });

    ABinderProcess_setThreadPoolMaxThreadCount(4);
    ABinderProcess_startThreadPool();

    auto composer = ndk::SharedRefBase::make<Composer>(tasks, config);
    makeBinderRealtime(composer->asBinder().get());
    const std::string instance = std::string(Composer::descriptor) + "/default";
    const binder_status_t status =
        AServiceManager_addService(composer->asBinder().get(), instance.c_str());
    if (status != STATUS_OK) {
        HWC_LOGE("cannot register %s with the service manager (status %d)", instance.c_str(), status);
        return 1;
    }
    HWC_LOGI("serving %s", instance.c_str());

    channel.start();
    ABinderProcess_joinThreadPool();
    return 1;
}
