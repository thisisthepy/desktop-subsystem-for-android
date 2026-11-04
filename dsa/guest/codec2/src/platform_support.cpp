// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The few platform functions the Codec 2.0 sources DSA builds call, which live in platform
// libraries an NDK build cannot link (libcodec2_vndk, libcodec2_hal_common). They behave as
// the platform's do.

#include <sys/system_properties.h>

#include <cstdlib>

#include <C2PlatformSupport.h>

namespace android {

int GetCodec2PoolMask() {
    char value[PROP_VALUE_MAX] = {};
    if (__system_property_get("debug.stagefright.c2-poolmask", value) > 0) {
        return static_cast<int>(strtol(value, nullptr, 0));
    }
    return 1 << C2PlatformAllocatorStore::ION | 1 << C2PlatformAllocatorStore::BUFFERQUEUE;
}

C2PlatformAllocatorStore::id_t GetPreferredLinearAllocatorId(int poolMask) {
    return ((poolMask >> C2PlatformAllocatorStore::BLOB) & 1) ? C2PlatformAllocatorStore::BLOB
                                                             : C2PlatformAllocatorStore::ION;
}

// DSA's codec HAL is an AIDL HAL.
bool IsCodec2AidlHalSelected() { return true; }

}  // namespace android
