// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#pragma once

#include <android/log.h>

#define DSA_HWC_TAG "dsa-hwcomposer"

#define HWC_LOGE(...) __android_log_print(ANDROID_LOG_ERROR, DSA_HWC_TAG, __VA_ARGS__)
#define HWC_LOGW(...) __android_log_print(ANDROID_LOG_WARN, DSA_HWC_TAG, __VA_ARGS__)
#define HWC_LOGI(...) __android_log_print(ANDROID_LOG_INFO, DSA_HWC_TAG, __VA_ARGS__)
#define HWC_LOGD(...) __android_log_print(ANDROID_LOG_DEBUG, DSA_HWC_TAG, __VA_ARGS__)
