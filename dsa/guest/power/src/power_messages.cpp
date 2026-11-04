// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "power_messages.h"

#include <string>

#include "control.pb.h"

namespace dsa::power {

namespace v1 = ::dsa::control::v1;

namespace {

std::vector<uint8_t> bytesOf(const v1::Envelope& envelope) {
    const std::string bytes = envelope.SerializeAsString();
    return std::vector<uint8_t>(bytes.begin(), bytes.end());
}

}  // namespace

std::optional<HostPower> decodeHostPower(const uint8_t* data, size_t size) {
    v1::Envelope envelope;
    if (!envelope.ParseFromArray(data, static_cast<int>(size)) ||
        envelope.body_case() != v1::Envelope::kPowerState) {
        return std::nullopt;
    }
    const v1::PowerState& state = envelope.power_state();
    HostPower power;
    power.acOnline = state.ac_online();
    power.batteryPresent = state.battery_present();
    power.batteryPercent = static_cast<int32_t>(state.battery_percent() > 100 ? 100 : state.battery_percent());
    power.charging = state.charging();
    power.powerSaver = state.power_saver();
    return power;
}

std::vector<uint8_t> encodeModeHint(int32_t mode, bool enabled) {
    v1::Envelope envelope;
    v1::PowerHint* hint = envelope.mutable_power_hint();
    hint->set_kind(v1::POWER_HINT_KIND_MODE);
    hint->set_id(mode);
    hint->set_enabled(enabled);
    return bytesOf(envelope);
}

std::vector<uint8_t> encodeBoostHint(int32_t boost, int32_t durationMs) {
    v1::Envelope envelope;
    v1::PowerHint* hint = envelope.mutable_power_hint();
    hint->set_kind(v1::POWER_HINT_KIND_BOOST);
    hint->set_id(boost);
    hint->set_duration_ms(durationMs);
    return bytesOf(envelope);
}

}  // namespace dsa::power
