// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The layout of libcutils' native_handle, which the platform's Codec 2.0 headers name as
// C2Handle. DSA's codec HAL reads handles through the binder NDK's NativeHandle and calls
// none of libcutils' functions.

#pragma once

typedef struct native_handle {
    int version;
    int numFds;
    int numInts;
    int data[0];
} native_handle_t;
