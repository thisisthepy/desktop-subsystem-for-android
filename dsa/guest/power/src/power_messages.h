// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The control channel messages the power and health HALs read and write, through the codec
// generated from dsa/protocol/control.proto. Each frame is one encoded Envelope.

#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <vector>

namespace dsa::power {

// The host computer's power, as the host reports it (PowerState).
struct HostPower {
    bool acOnline = true;
    bool batteryPresent = false;
    int32_t batteryPercent = 100;
    bool charging = false;
    bool powerSaver = false;

    bool operator==(const HostPower& other) const {
        return acOnline == other.acOnline && batteryPresent == other.batteryPresent &&
               batteryPercent == other.batteryPercent && charging == other.charging &&
               powerSaver == other.powerSaver;
    }
};

// The host's power, if the frame carries a PowerState; nothing for any other frame and for
// a frame that cannot be decoded.
std::optional<HostPower> decodeHostPower(const uint8_t* data, size_t size);

// An Envelope carrying a PowerHint that turns power mode `mode` on or off.
std::vector<uint8_t> encodeModeHint(int32_t mode, bool enabled);

// An Envelope carrying a PowerHint that asks for boost `boost` for `durationMs`.
std::vector<uint8_t> encodeBoostHint(int32_t boost, int32_t durationMs);

}  // namespace dsa::power
