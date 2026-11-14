// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "app_outputs.h"

namespace dsa::audio {

int32_t appSlotOfMixPort(int32_t portId) {
    const int32_t offset = portId - kFirstAppPortId;
    if (offset < 0 || offset % 2 != 0 || offset / 2 >= kAppOutputs) return -1;
    return offset / 2;
}

int32_t appSlotOfDevicePort(int32_t portId) {
    const int32_t offset = portId - kFirstAppPortId - 1;
    if (offset < 0 || offset % 2 != 0 || offset / 2 >= kAppOutputs) return -1;
    return offset / 2;
}

std::string streamName(int32_t mixPortId) {
    const int32_t slot = appSlotOfMixPort(mixPortId);
    if (slot < 0) return "primary";
    return "app " + std::to_string(slot);
}

std::string appDeviceAddress(int32_t slot) { return "dsa_app_" + std::to_string(slot); }

bool gainInRange(int32_t millibels) { return millibels >= kGainMinMb && millibels <= kGainMaxMb; }

}  // namespace dsa::audio
