/*
 * Copyright 2018 The Android Open Source Project
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef ANDROID_VNDK_NATIVEWINDOW_AHARDWAREBUFFER_H
#define ANDROID_VNDK_NATIVEWINDOW_AHARDWAREBUFFER_H

#include <android/hardware_buffer.h>
#include <cutils/native_handle.h>

__BEGIN_DECLS

const native_handle_t* AHardwareBuffer_getNativeHandle(const AHardwareBuffer* buffer);

enum CreateFromHandleMethod {
    AHARDWAREBUFFER_CREATE_FROM_HANDLE_METHOD_REGISTER = 2,
    AHARDWAREBUFFER_CREATE_FROM_HANDLE_METHOD_CLONE = 3,
};

int AHardwareBuffer_createFromHandle(const AHardwareBuffer_Desc* desc,
                                     const native_handle_t* handle, int32_t method,
                                     AHardwareBuffer** outBuffer);

#ifndef AHARDWAREBUFFER_FORMAT_YCbCr_P010
#define AHARDWAREBUFFER_FORMAT_YCbCr_P010 0x36
#endif

enum {
    AHARDWAREBUFFER_FORMAT_B8G8R8A8_UNORM           = 5,
    AHARDWAREBUFFER_FORMAT_YV12                     = 0x32315659,
    AHARDWAREBUFFER_FORMAT_Y8                       = 0x20203859,
    AHARDWAREBUFFER_FORMAT_Y16                      = 0x20363159,
    AHARDWAREBUFFER_FORMAT_RAW16                    = 0x20,
    AHARDWAREBUFFER_FORMAT_RAW10                    = 0x25,
    AHARDWAREBUFFER_FORMAT_RAW12                    = 0x26,
    AHARDWAREBUFFER_FORMAT_RAW_OPAQUE               = 0x24,
    AHARDWAREBUFFER_FORMAT_IMPLEMENTATION_DEFINED   = 0x22,
    AHARDWAREBUFFER_FORMAT_YCbCr_422_SP             = 0x10,
    AHARDWAREBUFFER_FORMAT_YCrCb_420_SP             = 0x11,
    AHARDWAREBUFFER_FORMAT_YCbCr_422_I              = 0x14,
};

enum {
    AHARDWAREBUFFER_USAGE_CAMERA_WRITE              = 2UL << 16,
    AHARDWAREBUFFER_USAGE_CAMERA_READ               = 4UL << 16,
    AHARDWAREBUFFER_USAGE_CAMERA_MASK               = 6UL << 16,
};

__END_DECLS

#endif /* ANDROID_VNDK_NATIVEWINDOW_AHARDWAREBUFFER_H */
