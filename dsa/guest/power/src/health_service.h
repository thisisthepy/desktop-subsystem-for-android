// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// dsa-health's IHealth (android.hardware.health, version 3). Android's battery service reads
// the battery here, and the battery is the host computer's: each PowerState the host sends
// changes it, and the battery service hears of each change through its callback. There is no
// charger, fuel gauge or storage to read, so what only those would know is not offered.

#pragma once

#include <memory>
#include <mutex>
#include <optional>
#include <vector>

#include <aidl/android/hardware/health/BnHealth.h>

#include "battery.h"
#include "power_messages.h"

namespace dsa::health {

class Health : public aidl::android::hardware::health::BnHealth {
   public:
    Health();

    // The host's power changed (or was told again).
    void hostPower(const power::HostPower& host);

    ndk::ScopedAStatus registerCallback(
        const std::shared_ptr<aidl::android::hardware::health::IHealthInfoCallback>& callback) override;
    ndk::ScopedAStatus unregisterCallback(
        const std::shared_ptr<aidl::android::hardware::health::IHealthInfoCallback>& callback) override;
    ndk::ScopedAStatus update() override;
    ndk::ScopedAStatus getChargeCounterUah(int32_t* value) override;
    ndk::ScopedAStatus getCurrentNowMicroamps(int32_t* value) override;
    ndk::ScopedAStatus getCurrentAverageMicroamps(int32_t* value) override;
    ndk::ScopedAStatus getCapacity(int32_t* value) override;
    ndk::ScopedAStatus getEnergyCounterNwh(int64_t* value) override;
    ndk::ScopedAStatus getChargeStatus(aidl::android::hardware::health::BatteryStatus* value) override;
    ndk::ScopedAStatus getStorageInfo(std::vector<aidl::android::hardware::health::StorageInfo>* value) override;
    ndk::ScopedAStatus getDiskStats(std::vector<aidl::android::hardware::health::DiskStats>* value) override;
    ndk::ScopedAStatus getHealthInfo(aidl::android::hardware::health::HealthInfo* value) override;
    ndk::ScopedAStatus setChargingPolicy(aidl::android::hardware::health::BatteryChargingPolicy value) override;
    ndk::ScopedAStatus getChargingPolicy(aidl::android::hardware::health::BatteryChargingPolicy* value) override;
    ndk::ScopedAStatus getBatteryHealthData(aidl::android::hardware::health::BatteryHealthData* value) override;

   private:
    aidl::android::hardware::health::HealthInfo infoLocked() const;
    void notify();

    std::mutex mLock;
    power::Battery mBattery;
    // The battery service's listeners. One whose process died is dropped the next time a
    // change does not reach it.
    std::vector<std::shared_ptr<aidl::android::hardware::health::IHealthInfoCallback>> mCallbacks;
};

}  // namespace dsa::health
