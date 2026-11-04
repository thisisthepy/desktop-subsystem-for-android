// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// libutils' systemTime, for the platform's Codec 2.0 sources.

#pragma once

#include <stdint.h>
#include <time.h>

typedef int64_t nsecs_t;

#define SYSTEM_TIME_MONOTONIC CLOCK_MONOTONIC

inline nsecs_t systemTime(int clock = SYSTEM_TIME_MONOTONIC) {
    timespec t{};
    clock_gettime(clock, &t);
    return static_cast<nsecs_t>(t.tv_sec) * 1000000000LL + t.tv_nsec;
}
