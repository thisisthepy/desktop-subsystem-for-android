// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The ALOG macros of the platform's liblog, for the platform's Codec 2.0 sources, on the
// NDK's liblog.

#pragma once

#include <android/log.h>

#ifndef LOG_TAG
#define LOG_TAG "dsa-codec2"
#endif

#define ALOGV(...) ((void)0)
#define ALOGD(...) __android_log_print(ANDROID_LOG_DEBUG, LOG_TAG, __VA_ARGS__)
#define ALOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
#define ALOGW(...) __android_log_print(ANDROID_LOG_WARN, LOG_TAG, __VA_ARGS__)
#define ALOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)
