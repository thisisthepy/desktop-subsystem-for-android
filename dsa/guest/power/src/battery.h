// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The guest's battery, as the health HAL reports it to Android, from the host computer's
// power. The guest has no battery of its own: it has the host's.
//
// - A host on its battery: the battery, its charge, and whether it charges.
// - A host without a battery (a desktop), or before the host has said: a full battery on AC
//   power, plugged in. Apps and the framework then behave as on a phone on its charger, and
//   nothing ever warns of a low battery that does not exist.

#pragma once

#include <cstdint>
#include <optional>

#include "power_messages.h"

namespace dsa::power {

// android.hardware.health's BatteryStatus.
enum class BatteryStatus : int32_t { Unknown = 1, Charging = 2, Discharging = 3, NotCharging = 4, Full = 5 };

// android.hardware.health's BatteryCapacityLevel.
enum class CapacityLevel : int32_t { Unknown = 0, Critical = 1, Low = 2, Normal = 3, High = 4, Full = 5 };

// android.hardware.health's BatteryHealth.GOOD and BatteryChargingState.NORMAL.
constexpr int32_t kBatteryHealthGood = 2;
constexpr int32_t kChargingStateNormal = 1;

struct Battery {
    bool acOnline = true;
    bool present = true;
    int32_t level = 100;
    BatteryStatus status = BatteryStatus::Full;
    CapacityLevel capacityLevel = CapacityLevel::Full;

    bool operator==(const Battery& other) const {
        return acOnline == other.acOnline && present == other.present && level == other.level &&
               status == other.status && capacityLevel == other.capacityLevel;
    }
};

// The battery the guest has while the host's power is `host` (nothing before the host has
// said).
Battery batteryFor(const std::optional<HostPower>& host);

}  // namespace dsa::power
