// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Android's log, for the codec HAL's tests on the build machine: each line goes to stderr.

#pragma once

#include <cstdarg>
#include <cstdio>

enum { ANDROID_LOG_INFO = 4, ANDROID_LOG_WARN = 5, ANDROID_LOG_ERROR = 6 };

inline int __android_log_print(int priority, const char* tag, const char* format, ...) {
    std::fprintf(stderr, "%d %s: ", priority, tag);
    va_list args;
    va_start(args, format);
    const int written = std::vfprintf(stderr, format, args);
    va_end(args);
    std::fputc('\n', stderr);
    return written;
}
