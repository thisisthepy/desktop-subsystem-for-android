// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The power and health HALs' rules: their channel messages against the checked-in protocol
// vectors, so they stay in step with the Rust and Kotlin sides; the battery the guest has for
// the host's power; and the modes and boosts the power HAL takes. The protocol folder is the
// only argument.

#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include "battery.h"
#include "power_messages.h"
#include "power_modes.h"
#include "test_main.h"

using namespace dsa::power;

namespace {

std::string gProtocolDir;

std::vector<uint8_t> vector(const std::string& name) {
    std::ifstream in(gProtocolDir + "/vectors/control.vectors");
    std::string line;
    while (std::getline(in, line)) {
        std::istringstream fields(line);
        std::string key, hex;
        fields >> key >> hex;
        if (key != name) {
            continue;
        }
        std::vector<uint8_t> bytes;
        for (size_t i = 0; hex != "-" && i + 1 < hex.size(); i += 2) {
            bytes.push_back(static_cast<uint8_t>(std::stoi(hex.substr(i, 2), nullptr, 16)));
        }
        return bytes;
    }
    ::dsa::test::fail(__FILE__, __LINE__, "the vectors have no case " + name);
    return {};
}

HostPower onBattery(int32_t percent, bool charging) {
    HostPower host;
    host.acOnline = charging;
    host.batteryPresent = true;
    host.batteryPercent = percent;
    host.charging = charging;
    return host;
}

}  // namespace

TEST(pr4_a_mode_hint_encodes_as_the_protocol_vector) {
    EXPECT_EQ(encodeModeHint(5, true), vector("power_hint_launch_mode"));
}

TEST(pr4_a_boost_hint_encodes_as_the_protocol_vector) {
    EXPECT_EQ(encodeBoostHint(0, 80), vector("power_hint_interaction_boost"));
}

TEST(pr4_the_hosts_power_state_decodes) {
    const std::vector<uint8_t> bytes = vector("power_state_on_battery");
    const auto host = decodeHostPower(bytes.data(), bytes.size());
    EXPECT_TRUE(host.has_value());
    if (host) {
        EXPECT_TRUE(!host->acOnline);
        EXPECT_TRUE(host->batteryPresent);
        EXPECT_EQ(host->batteryPercent, 57);
        EXPECT_TRUE(!host->charging);
        EXPECT_TRUE(host->powerSaver);
    }
}

TEST(pr4_frames_other_than_the_power_state_are_not_the_hosts_power) {
    const std::vector<uint8_t> bytes = vector("task_removed");
    EXPECT_TRUE(!decodeHostPower(bytes.data(), bytes.size()).has_value());
    const uint8_t garbage[] = {0xff, 0xff, 0xff};
    EXPECT_TRUE(!decodeHostPower(garbage, sizeof garbage).has_value());
}

TEST(pr4_before_the_host_says_the_battery_is_full_on_ac_power) {
    const Battery battery = batteryFor(std::nullopt);
    EXPECT_TRUE(battery.acOnline);
    EXPECT_TRUE(battery.present);
    EXPECT_EQ(battery.level, 100);
    EXPECT_TRUE(battery.status == BatteryStatus::Full);
    EXPECT_TRUE(battery.capacityLevel == CapacityLevel::Full);
}

TEST(pr4_a_host_without_a_battery_is_plugged_in_and_full) {
    HostPower desktop;
    desktop.acOnline = true;
    desktop.batteryPresent = false;
    desktop.batteryPercent = 100;
    EXPECT_TRUE(batteryFor(desktop) == batteryFor(std::nullopt));
}

TEST(pr4_a_host_on_its_battery_discharges_at_its_charge) {
    const Battery battery = batteryFor(onBattery(57, false));
    EXPECT_TRUE(!battery.acOnline);
    EXPECT_EQ(battery.level, 57);
    EXPECT_TRUE(battery.status == BatteryStatus::Discharging);
    EXPECT_TRUE(battery.capacityLevel == CapacityLevel::Normal);
}

TEST(pr4_a_charging_host_charges_and_a_full_one_on_ac_is_full) {
    EXPECT_TRUE(batteryFor(onBattery(40, true)).status == BatteryStatus::Charging);
    HostPower full = onBattery(100, false);
    full.acOnline = true;
    EXPECT_TRUE(batteryFor(full).status == BatteryStatus::Full);
    HostPower held = onBattery(80, false);
    held.acOnline = true;
    EXPECT_TRUE(batteryFor(held).status == BatteryStatus::NotCharging);
}

TEST(pr4_a_low_charge_is_reported_low_and_then_critical) {
    EXPECT_TRUE(batteryFor(onBattery(10, false)).capacityLevel == CapacityLevel::Low);
    EXPECT_TRUE(batteryFor(onBattery(3, false)).capacityLevel == CapacityLevel::Critical);
    EXPECT_TRUE(batteryFor(onBattery(90, false)).capacityLevel == CapacityLevel::High);
}

TEST(pr4_the_hal_takes_the_modes_and_boosts_the_host_acts_on) {
    // LAUNCH, GAME, LOW_POWER, INTERACTIVE, DISPLAY_INACTIVE.
    for (int32_t mode : {5, 15, 1, 7, 9}) {
        EXPECT_TRUE(modeSupported(mode));
    }
    // DOUBLE_TAP_TO_WAKE, the camera streaming modes, DISPLAY_CHANGE, AUTOMOTIVE_PROJECTION.
    for (int32_t mode : {0, 11, 12, 13, 14, 17, 18}) {
        EXPECT_TRUE(!modeSupported(mode));
    }
    EXPECT_TRUE(boostSupported(0));
    EXPECT_TRUE(boostSupported(1));
    // ML_ACC.
    EXPECT_TRUE(!boostSupported(2));
}

TEST(pr4_a_mode_set_again_changes_nothing) {
    ModeSet modes;
    EXPECT_TRUE(modes.set(5, true));
    EXPECT_TRUE(!modes.set(5, true));
    EXPECT_TRUE(modes.set(15, true));
    EXPECT_EQ(modes.on(), (std::vector<int32_t>{5, 15}));
    EXPECT_TRUE(modes.set(5, false));
    EXPECT_TRUE(!modes.set(5, false));
    EXPECT_EQ(modes.on(), (std::vector<int32_t>{15}));
}

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s <dsa/protocol>\n", argv[0]);
        return 2;
    }
    gProtocolDir = argv[1];
    return ::dsa::test::runAll();
}
