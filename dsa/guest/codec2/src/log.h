// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#pragma once

#include <android/log.h>

#define DSA_CODEC_TAG "dsa-codec2"

#define CODEC_LOGE(...) __android_log_print(ANDROID_LOG_ERROR, DSA_CODEC_TAG, __VA_ARGS__)
#define CODEC_LOGW(...) __android_log_print(ANDROID_LOG_WARN, DSA_CODEC_TAG, __VA_ARGS__)
#define CODEC_LOGI(...) __android_log_print(ANDROID_LOG_INFO, DSA_CODEC_TAG, __VA_ARGS__)
