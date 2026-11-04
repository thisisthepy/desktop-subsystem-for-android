// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// libbase's HexString, for the platform's Codec 2.0 sources (see logging.h).

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>

namespace android {
namespace base {

inline std::string HexString(const void* bytes, size_t len) {
    static const char kDigits[] = "0123456789abcdef";
    const auto* p = static_cast<const uint8_t*>(bytes);
    std::string out;
    out.reserve(len * 2);
    for (size_t i = 0; i < len; ++i) {
        out.push_back(kDigits[p[i] >> 4]);
        out.push_back(kDigits[p[i] & 0xf]);
    }
    return out;
}

}  // namespace base
}  // namespace android
