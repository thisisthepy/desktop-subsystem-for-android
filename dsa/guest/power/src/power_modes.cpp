// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "power_modes.h"

#include <algorithm>
#include <array>

namespace dsa::power {

namespace {

// LOW_POWER, SUSTAINED_PERFORMANCE, FIXED_PERFORMANCE, VR, LAUNCH, EXPENSIVE_RENDERING,
// INTERACTIVE, DEVICE_IDLE, DISPLAY_INACTIVE, AUDIO_STREAMING_LOW_LATENCY, GAME, GAME_LOADING.
constexpr std::array<int32_t, 12> kModes = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 15, 16};
// INTERACTION, DISPLAY_UPDATE_IMMINENT, AUDIO_LAUNCH, CAMERA_LAUNCH, CAMERA_SHOT.
constexpr std::array<int32_t, 5> kBoosts = {0, 1, 3, 4, 5};

}  // namespace

bool modeSupported(int32_t mode) { return std::find(kModes.begin(), kModes.end(), mode) != kModes.end(); }

bool boostSupported(int32_t boost) {
    return std::find(kBoosts.begin(), kBoosts.end(), boost) != kBoosts.end();
}

bool ModeSet::set(int32_t mode, bool enabled) {
    std::lock_guard<std::mutex> lock(mLock);
    if (enabled) {
        return mOn.insert(mode).second;
    }
    return mOn.erase(mode) > 0;
}

std::vector<int32_t> ModeSet::on() const {
    std::lock_guard<std::mutex> lock(mLock);
    return std::vector<int32_t>(mOn.begin(), mOn.end());
}

}  // namespace dsa::power
