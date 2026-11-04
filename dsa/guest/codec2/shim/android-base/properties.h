// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// libbase's property getters, for the platform's Codec 2.0 sources (see logging.h), on the
// NDK's __system_property_get.

#pragma once

#include <sys/system_properties.h>

#include <cstdint>
#include <cstdlib>
#include <string>

namespace android {
namespace base {

inline std::string GetProperty(const std::string& key, const std::string& defaultValue) {
    char value[PROP_VALUE_MAX] = {};
    if (__system_property_get(key.c_str(), value) <= 0) {
        return defaultValue;
    }
    return value;
}

template <typename T>
T GetIntProperty(const std::string& key, T defaultValue) {
    const std::string value = GetProperty(key, "");
    if (value.empty()) {
        return defaultValue;
    }
    char* end = nullptr;
    const long long parsed = strtoll(value.c_str(), &end, 0);
    return end && *end == '\0' ? static_cast<T>(parsed) : defaultValue;
}

inline bool GetBoolProperty(const std::string& key, bool defaultValue) {
    const std::string value = GetProperty(key, "");
    if (value == "1" || value == "y" || value == "yes" || value == "on" || value == "true") return true;
    if (value == "0" || value == "n" || value == "no" || value == "off" || value == "false") return false;
    return defaultValue;
}

}  // namespace base
}  // namespace android
