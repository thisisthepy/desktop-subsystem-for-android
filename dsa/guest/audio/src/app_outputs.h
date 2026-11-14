// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The module's outputs and the volume they carry, apart from binder so they can be tested on
// the build machine.
//
// Besides the shared output (the primary mix port and the speaker), the module has one
// output for each app with a window: a mix port routed to a bus device of its own, whose
// address the agent routes that app's uid to with an audio policy. Each output is its own
// stream on the host's audio port, named so the host plays it as its app ("app 0", "app 1",
// ...; the shared one is "primary").
//
// The volume is the host's alone. Every output device has a gain the framework uses for
// volume (AudioGain.useForVolume), so audio policy sets the volume as the device's gain and
// leaves the mixer at full scale; the module keeps the gain it was given and never applies it
// to the sound. Android's media volume still works for apps, and reaches the host through the
// agent instead, as the app's volume in the host's own mixer.

#pragma once

#include <cstdint>
#include <string>

namespace dsa::audio {

// The outputs for apps. The agent gives each app with a window one, and every other app
// plays on the shared output.
constexpr int32_t kAppOutputs = 8;

// The port IDs of the app outputs: a mix port and its bus device for each, from here up.
constexpr int32_t kFirstAppPortId = 10;

constexpr int32_t appMixPortId(int32_t slot) { return kFirstAppPortId + 2 * slot; }
constexpr int32_t appDevicePortId(int32_t slot) { return kFirstAppPortId + 2 * slot + 1; }

// The app output whose mix port is `portId`, or -1 for any other port.
int32_t appSlotOfMixPort(int32_t portId);

// The app output whose bus device is `portId`, or -1 for any other port.
int32_t appSlotOfDevicePort(int32_t portId);

// The name the host's audio port knows a stream on mix port `mixPortId` by: "app <slot>" for
// an app output, "primary" for the shared one.
std::string streamName(int32_t mixPortId);

// The address of app output `slot`'s bus device, which the agent routes an app's uid to.
std::string appDeviceAddress(int32_t slot);

// The gain of every output device, in millibels: -96 dB to 0 dB in steps of 1 dB. Audio
// policy sets a stream's volume here instead of in the mixer.
constexpr int32_t kGainMinMb = -9600;
constexpr int32_t kGainMaxMb = 0;
constexpr int32_t kGainStepMb = 100;

// Whether `millibels` is a gain an output device takes.
bool gainInRange(int32_t millibels);

}  // namespace dsa::audio
