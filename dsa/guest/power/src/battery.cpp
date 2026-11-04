// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "battery.h"

namespace dsa::power {

namespace {

// The levels Android's own health HAL reports for a charge (healthd's thresholds).
CapacityLevel levelOf(int32_t percent) {
    if (percent >= 100) {
        return CapacityLevel::Full;
    }
    if (percent >= 80) {
        return CapacityLevel::High;
    }
    if (percent > 15) {
        return CapacityLevel::Normal;
    }
    if (percent > 5) {
        return CapacityLevel::Low;
    }
    return CapacityLevel::Critical;
}

}  // namespace

Battery batteryFor(const std::optional<HostPower>& host) {
    Battery battery;
    if (!host || !host->batteryPresent) {
        return battery;
    }
    const int32_t percent = host->batteryPercent < 0 ? 0 : (host->batteryPercent > 100 ? 100 : host->batteryPercent);
    battery.acOnline = host->acOnline;
    battery.level = percent;
    battery.capacityLevel = levelOf(percent);
    if (host->charging) {
        battery.status = BatteryStatus::Charging;
    } else if (host->acOnline && percent >= 100) {
        battery.status = BatteryStatus::Full;
    } else if (host->acOnline) {
        battery.status = BatteryStatus::NotCharging;
    } else {
        battery.status = BatteryStatus::Discharging;
    }
    return battery;
}

}  // namespace dsa::power
