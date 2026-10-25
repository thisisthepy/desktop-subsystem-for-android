// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// android::base::GetProperty for the guest Vulkan driver. The driver is built with the
// NDK's libc++, whose std::string is mangled differently from the platform libbase.so's,
// so it cannot call that library's GetProperty. This one reads the property through libc.

#include <string>

#include <sys/system_properties.h>

namespace android {
namespace base {

std::string GetProperty(const std::string& key, const std::string& default_value) {
    char value[PROP_VALUE_MAX] = {};
    if (__system_property_get(key.c_str(), value) > 0) {
        return value;
    }
    return default_value;
}

}  // namespace base
}  // namespace android
