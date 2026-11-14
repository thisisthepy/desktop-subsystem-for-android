// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The module's app outputs: each has a mix port and a bus device of its own, the host knows
// its stream by the app output's name, and the volume audio policy sets is a gain the module
// takes and never applies to the sound.

#include "app_outputs.h"
#include "test_main.h"

using namespace dsa::audio;

TEST(fr26_each_app_output_has_a_mix_port_and_a_bus_device_of_its_own) {
    for (int32_t slot = 0; slot < kAppOutputs; ++slot) {
        EXPECT_EQ(appSlotOfMixPort(appMixPortId(slot)), slot);
        EXPECT_EQ(appSlotOfDevicePort(appDevicePortId(slot)), slot);
        EXPECT_EQ(appSlotOfMixPort(appDevicePortId(slot)), -1);
        EXPECT_EQ(appSlotOfDevicePort(appMixPortId(slot)), -1);
    }
    // The shared output's and the microphone's ports are none of them.
    for (int32_t port = 0; port < kFirstAppPortId; ++port) {
        EXPECT_EQ(appSlotOfMixPort(port), -1);
        EXPECT_EQ(appSlotOfDevicePort(port), -1);
    }
    EXPECT_EQ(appSlotOfMixPort(appMixPortId(kAppOutputs)), -1);
    EXPECT_TRUE(appDeviceAddress(3) == "dsa_app_3");
}

TEST(fr26_the_host_knows_each_output_by_its_apps_name_and_the_shared_one_as_primary) {
    EXPECT_TRUE(streamName(1) == "primary");
    EXPECT_TRUE(streamName(appMixPortId(0)) == "app 0");
    EXPECT_TRUE(streamName(appMixPortId(7)) == "app 7");
}

TEST(fr26_the_volume_is_a_gain_from_minus_96_db_to_full_scale) {
    EXPECT_TRUE(gainInRange(0));
    EXPECT_TRUE(gainInRange(-1500));
    EXPECT_TRUE(gainInRange(kGainMinMb));
    // Audio policy never makes the sound louder than the app made it.
    EXPECT_TRUE(!gainInRange(100));
    EXPECT_TRUE(!gainInRange(kGainMinMb - 1));
    EXPECT_EQ((kGainMaxMb - kGainMinMb) % kGainStepMb, 0);
}

int main() { return dsa::test::runAll(); }
