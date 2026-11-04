// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// dsa-codec2: the guest's Codec 2.0 HAL for video decoding on the host's GPU.

#include <android/binder_manager.h>
#include <android/binder_process.h>

#include <csignal>
#include <string>

#include "log.h"
#include "store.h"

int main() {
    // A host connection that drops must not kill the process.
    signal(SIGPIPE, SIG_IGN);

    ABinderProcess_setThreadPoolMaxThreadCount(8);
    ABinderProcess_startThreadPool();

    auto store = ndk::SharedRefBase::make<dsa::codec2::Store>();
    store->start();
    const std::string instance = std::string(dsa::codec2::Store::descriptor) + "/default";
    const binder_status_t status = AServiceManager_addService(store->asBinder().get(), instance.c_str());
    if (status != STATUS_OK) {
        CODEC_LOGE("cannot register %s with the service manager (status %d)", instance.c_str(), status);
        return 1;
    }
    CODEC_LOGI("serving %s; video decodes on the host's GPU", instance.c_str());
    ABinderProcess_joinThreadPool();
    return 1;
}
