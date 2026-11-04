// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// libbase's StringPrintf, for the platform's Codec 2.0 sources (see logging.h).

#pragma once

#include <cstdarg>
#include <cstdio>
#include <string>

namespace android {
namespace base {

inline std::string StringPrintf(const char* format, ...) __attribute__((format(printf, 1, 2)));

inline std::string StringPrintf(const char* format, ...) {
    va_list args;
    va_start(args, format);
    va_list copy;
    va_copy(copy, args);
    const int size = vsnprintf(nullptr, 0, format, copy);
    va_end(copy);
    std::string out;
    if (size > 0) {
        out.resize(static_cast<size_t>(size) + 1);
        vsnprintf(&out[0], out.size(), format, args);
        out.resize(static_cast<size_t>(size));
    }
    va_end(args);
    return out;
}

}  // namespace base
}  // namespace android
