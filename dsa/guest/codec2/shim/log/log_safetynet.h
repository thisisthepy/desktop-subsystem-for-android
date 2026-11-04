// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The platform's SafetyNet event log is not reachable from an NDK build; its reports go to
// the ordinary log instead.

#pragma once

#include <android/log.h>

#define android_errorWriteLog(tag, subTag) \
    __android_log_print(ANDROID_LOG_WARN, "dsa-codec2", "safetynet %x %s", (unsigned)(tag), (subTag))
