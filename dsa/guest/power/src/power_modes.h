// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The power modes and boosts the power HAL takes, and the modes that are on. The host acts
// on these (it runs the guest's processors at full speed, as any program, or frugally), so
// the HAL reports these as supported and the framework sends no others. The lists match the
// host's.

#pragma once

#include <cstdint>
#include <mutex>
#include <set>
#include <vector>

namespace dsa::power {

// android.hardware.power's Mode values the host acts on.
bool modeSupported(int32_t mode);
// android.hardware.power's Boost values the host acts on.
bool boostSupported(int32_t boost);

// The modes that are on, so they are told again to a channel that comes back.
class ModeSet {
   public:
    // Records `mode` on or off. Returns whether that changed anything.
    bool set(int32_t mode, bool enabled);
    std::vector<int32_t> on() const;

   private:
    mutable std::mutex mLock;
    std::set<int32_t> mOn;
};

}  // namespace dsa::power
