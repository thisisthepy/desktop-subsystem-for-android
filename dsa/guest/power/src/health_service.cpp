// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "health_service.h"

#include <algorithm>

#include "log.h"

namespace dsa::health {

using aidl::android::hardware::health::BatteryCapacityLevel;
using aidl::android::hardware::health::BatteryChargingPolicy;
using aidl::android::hardware::health::BatteryChargingState;
using aidl::android::hardware::health::BatteryHealth;
using aidl::android::hardware::health::BatteryHealthData;
using aidl::android::hardware::health::BatteryStatus;
using aidl::android::hardware::health::DiskStats;
using aidl::android::hardware::health::HealthInfo;
using aidl::android::hardware::health::IHealthInfoCallback;
using aidl::android::hardware::health::StorageInfo;

namespace {

ndk::ScopedAStatus unsupported() { return ndk::ScopedAStatus::fromExceptionCode(EX_UNSUPPORTED_OPERATION); }

// A room's temperature and a cell's voltage: the battery service reads these, and warns of a
// battery that is too hot, cold or flat.
constexpr int32_t kTemperatureTenthsCelsius = 250;
constexpr int32_t kVoltageMillivolts = 4200;

}  // namespace

Health::Health()
    : mBattery(power::batteryFor(std::nullopt)) {}

HealthInfo Health::infoLocked() const {
    HealthInfo info;
    info.chargerAcOnline = mBattery.acOnline;
    info.chargerUsbOnline = false;
    info.chargerWirelessOnline = false;
    info.chargerDockOnline = false;
    info.batteryStatus = static_cast<BatteryStatus>(mBattery.status);
    info.batteryHealth = static_cast<BatteryHealth>(power::kBatteryHealthGood);
    info.batteryPresent = mBattery.present;
    info.batteryLevel = mBattery.level;
    info.batteryVoltageMillivolts = kVoltageMillivolts;
    info.batteryTemperatureTenthsCelsius = kTemperatureTenthsCelsius;
    info.batteryTechnology = "Li-ion";
    info.batteryCapacityLevel = static_cast<BatteryCapacityLevel>(mBattery.capacityLevel);
    info.batteryChargeTimeToFullNowSeconds = HealthInfo::BATTERY_CHARGE_TIME_TO_FULL_NOW_SECONDS_UNSUPPORTED;
    info.chargingState = static_cast<BatteryChargingState>(power::kChargingStateNormal);
    info.chargingPolicy = BatteryChargingPolicy::DEFAULT;
    return info;
}

void Health::hostPower(const power::HostPower& host) {
    {
        std::lock_guard<std::mutex> lock(mLock);
        const power::Battery battery = power::batteryFor(host);
        if (battery == mBattery) {
            return;
        }
        mBattery = battery;
        POWER_LOGI("battery now %d%%, %s", battery.level, battery.acOnline ? "on AC power" : "on battery");
    }
    notify();
}

void Health::notify() {
    std::vector<std::shared_ptr<IHealthInfoCallback>> callbacks;
    HealthInfo info;
    {
        std::lock_guard<std::mutex> lock(mLock);
        callbacks = mCallbacks;
        info = infoLocked();
    }
    std::vector<std::shared_ptr<IHealthInfoCallback>> dead;
    for (const auto& callback : callbacks) {
        const ndk::ScopedAStatus status = callback->healthInfoChanged(info);
        if (status.getStatus() == STATUS_DEAD_OBJECT) {
            dead.push_back(callback);
        } else if (!status.isOk()) {
            POWER_LOGW("a battery listener did not take the change: %s", status.getDescription().c_str());
        }
    }
    if (!dead.empty()) {
        // A listener whose process died is forgotten.
        std::lock_guard<std::mutex> lock(mLock);
        mCallbacks.erase(std::remove_if(mCallbacks.begin(), mCallbacks.end(),
                                        [&](const auto& c) {
                                            return std::find(dead.begin(), dead.end(), c) != dead.end();
                                        }),
                         mCallbacks.end());
    }
}

ndk::ScopedAStatus Health::registerCallback(const std::shared_ptr<IHealthInfoCallback>& callback) {
    if (!callback) {
        return ndk::ScopedAStatus::fromExceptionCode(EX_NULL_POINTER);
    }
    HealthInfo info;
    {
        std::lock_guard<std::mutex> lock(mLock);
        mCallbacks.push_back(callback);
        info = infoLocked();
    }
    // A new listener hears the battery at once, as the platform's own health HAL does.
    const ndk::ScopedAStatus status = callback->healthInfoChanged(info);
    if (!status.isOk()) {
        POWER_LOGW("a new battery listener did not take the battery: %s", status.getDescription().c_str());
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Health::unregisterCallback(const std::shared_ptr<IHealthInfoCallback>& callback) {
    if (!callback) {
        return ndk::ScopedAStatus::fromExceptionCode(EX_NULL_POINTER);
    }
    std::lock_guard<std::mutex> lock(mLock);
    const auto before = mCallbacks.size();
    mCallbacks.erase(std::remove_if(mCallbacks.begin(), mCallbacks.end(),
                                    [&](const auto& c) { return c->asBinder() == callback->asBinder(); }),
                     mCallbacks.end());
    if (mCallbacks.size() == before) {
        return ndk::ScopedAStatus::fromServiceSpecificError(STATUS_UNKNOWN);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Health::update() {
    notify();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Health::getChargeCounterUah(int32_t*) { return unsupported(); }
ndk::ScopedAStatus Health::getCurrentNowMicroamps(int32_t*) { return unsupported(); }
ndk::ScopedAStatus Health::getCurrentAverageMicroamps(int32_t*) { return unsupported(); }
ndk::ScopedAStatus Health::getEnergyCounterNwh(int64_t*) { return unsupported(); }

ndk::ScopedAStatus Health::getCapacity(int32_t* value) {
    std::lock_guard<std::mutex> lock(mLock);
    *value = mBattery.level;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Health::getChargeStatus(BatteryStatus* value) {
    std::lock_guard<std::mutex> lock(mLock);
    *value = static_cast<BatteryStatus>(mBattery.status);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Health::getStorageInfo(std::vector<StorageInfo>*) { return unsupported(); }
ndk::ScopedAStatus Health::getDiskStats(std::vector<DiskStats>*) { return unsupported(); }

ndk::ScopedAStatus Health::getHealthInfo(HealthInfo* value) {
    std::lock_guard<std::mutex> lock(mLock);
    *value = infoLocked();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Health::setChargingPolicy(BatteryChargingPolicy) { return unsupported(); }

ndk::ScopedAStatus Health::getChargingPolicy(BatteryChargingPolicy* value) {
    *value = BatteryChargingPolicy::DEFAULT;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Health::getBatteryHealthData(BatteryHealthData*) { return unsupported(); }

}  // namespace dsa::health
