// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Logging for the power and health HALs. Each binary sets LOG_TAG (dsa-power, dsa-health).

#pragma once

#include <android/log.h>

#define POWER_LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)
#define POWER_LOGW(...) __android_log_print(ANDROID_LOG_WARN, LOG_TAG, __VA_ARGS__)
#define POWER_LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
