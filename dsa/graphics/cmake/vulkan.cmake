# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0
#
# The guest Vulkan driver (vulkan.ranchu), built from the same sources AOSP's Android.bp
# links into it. Mesa's shared code (the Vulkan runtime, util, WSI and their generated
# sources) comes from Mesa's own Meson build in MESA_BUILD_DIR; this file compiles the
# gfxstream guest parts that Meson only builds for Linux guests.

if(NOT DEFINED MESA_BUILD_DIR)
    set(MESA_BUILD_DIR "${SCRATCH_DIR}/build-mesa")
endif()
get_filename_component(MESA_BUILD_DIR "${MESA_BUILD_DIR}" ABSOLUTE)

if(NOT DEFINED FRAMEWORKS_NATIVE_DIR)
    set(FRAMEWORKS_NATIVE_DIR "${SCRATCH_DIR}/frameworks_native")
endif()

set(MESA_GUEST "${MESA3D_DIR}/src/gfxstream/guest")
set(GFXSTREAM_GUEST "${GFXSTREAM_DIR}/guest")

set(VK_RANCHU_SOURCES
    ${MESA_GUEST}/vulkan/gfxstream_vk_device.cpp
    ${MESA_GUEST}/vulkan/gfxstream_vk_cmd.cpp
    ${MESA_GUEST}/vulkan/gfxstream_vk_wsi.cpp
    ${MESA_GUEST}/vulkan/gfxstream_vk_android.cpp
    ${MESA_GUEST}/vulkan_enc/AndroidHardwareBuffer.cpp
    ${MESA_GUEST}/vulkan_enc/CommandBufferStagingStream.cpp
    ${MESA_GUEST}/vulkan_enc/DescriptorSetVirtualization.cpp
    ${MESA_GUEST}/vulkan_enc/HostVisibleMemoryVirtualization.cpp
    ${MESA_GUEST}/vulkan_enc/Resources.cpp
    ${MESA_GUEST}/vulkan_enc/ResourceTracker.cpp
    ${MESA_GUEST}/vulkan_enc/Validation.cpp
    ${MESA_GUEST}/vulkan_enc/VulkanHandleMapping.cpp
    ${MESA_GUEST}/vulkan_enc/VulkanStreamGuest.cpp
    ${MESA_GUEST}/vulkan_enc/gfxstream_vk_private.cpp
    ${MESA_GUEST}/vulkan_enc/GfxStreamVulkanConnection.cpp
    ${MESA_GUEST}/vulkan_enc/goldfish_vk_counting_guest.cpp
    ${MESA_GUEST}/vulkan_enc/goldfish_vk_deepcopy_guest.cpp
    ${MESA_GUEST}/vulkan_enc/goldfish_vk_extension_structs_guest.cpp
    ${MESA_GUEST}/vulkan_enc/goldfish_vk_marshaling_guest.cpp
    ${MESA_GUEST}/vulkan_enc/goldfish_vk_reserved_marshaling_guest.cpp
    ${MESA_GUEST}/vulkan_enc/goldfish_vk_transform_guest.cpp
    ${MESA_GUEST}/vulkan_enc/VkEncoder.cpp
    ${MESA_GUEST}/vulkan_enc/func_table.cpp
    ${MESA_BUILD_DIR}/src/gfxstream/guest/vulkan_enc/gfxstream_vk_entrypoints.c
    ${MESA_GUEST}/android/ANativeWindowAndroid.cpp
    ${MESA_GUEST}/android/GfxStreamGralloc.cpp
    ${MESA_GUEST}/android/GrallocGoldfish.cpp
    ${MESA_GUEST}/android/GrallocMinigbm.cpp
    ${MESA_GUEST}/connection-manager/GfxStreamConnectionManager.cpp
    ${MESA_GUEST}/connection-manager/GfxStreamConnection.cpp
    ${MESA_GUEST}/connection-manager/QemuPipeStream.cpp
    ${MESA_GUEST}/connection-manager/VirtioGpuPipeStream.cpp
    ${MESA_GUEST}/platform/VirtGpu.cpp
    ${MESA_GUEST}/platform/linux/LinuxVirtGpuBlob.cpp
    ${MESA_GUEST}/platform/linux/LinuxVirtGpuBlobMapping.cpp
    ${MESA_GUEST}/platform/linux/LinuxVirtGpuDevice.cpp
    ${MESA_GUEST}/platform/linux/LinuxSync.cpp
    ${MESA_GUEST}/platform/kumquat/VirtGpuKumquatStub.cpp
    ${MESA_GUEST}/GoldfishAddressSpace/AddressSpaceStream.cpp
    ${MESA_GUEST}/GoldfishAddressSpace/goldfish_address_space.cpp
    ${MESA_GUEST}/GoldfishAddressSpace/VirtioGpuAddressSpaceStream.cpp
    ${MESA_GUEST}/GoldfishAddressSpace/GoldfishAddressSpaceStream.cpp
    ${GFXSTREAM_GUEST}/OpenglSystemCommon/HvSocketStream.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/renderControl_client_context.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/renderControl_enc.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/renderControl_entry.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/ExtendedRenderControl.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/GfxStreamRenderControl.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/GfxStreamRenderControlConnection.cpp
    ${GFXSTREAM_GUEST}/OpenglCodecCommon/ChecksumCalculator.cpp
    ${GFXSTREAM_GUEST}/OpenglCodecCommon/glUtils.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/AlignedBuf.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/files/MemStream.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/files/Stream.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/files/StreamSerializing.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/Path.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/Pool.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/StringFormat.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/Process.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/synchronization/AndroidMessageChannel.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/threads/AndroidFunctorThread.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/threads/AndroidThreadStore.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/threads/AndroidThread_pthread.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/threads/AndroidWorkPool.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/Tracing.cpp
    ${GFXSTREAM_GUEST}/android-emu/android/utils/debug.c
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/ring_buffer.c
    ${GFXSTREAM_GUEST}/qemupipe/qemu_pipe_common.cpp
    ${GFXSTREAM_GUEST}/qemupipe/qemu_pipe_guest.cpp
    ${CMAKE_CURRENT_SOURCE_DIR}/android_properties.cpp
)

add_library(nativewindow_stub SHARED ${CMAKE_CURRENT_SOURCE_DIR}/nativewindow_stub.c)
set_target_properties(nativewindow_stub PROPERTIES
    OUTPUT_NAME "nativewindow"
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/platform-stubs"
)

add_library(vulkan.ranchu SHARED ${VK_RANCHU_SOURCES})

target_include_directories(vulkan.ranchu PRIVATE
    ${MESA_GUEST}/vulkan
    ${MESA_GUEST}/vulkan_enc
    ${MESA_GUEST}/android
    ${MESA_GUEST}/android/include
    ${MESA_GUEST}/connection-manager
    ${MESA_GUEST}/platform/include
    ${MESA_GUEST}/platform
    ${MESA_GUEST}/GoldfishAddressSpace/include
    ${MESA_GUEST}/iostream/include
    ${MESA3D_DIR}/src/gfxstream/aemu/include
    ${MESA_BUILD_DIR}/src/gfxstream/guest/vulkan_enc
    ${MESA_BUILD_DIR}/src/vulkan/runtime
    ${MESA_BUILD_DIR}/src/vulkan/util
    ${MESA_BUILD_DIR}/src/compiler
    ${MESA_BUILD_DIR}/src/compiler/spirv
    ${MESA_BUILD_DIR}/src/util
    ${MESA_BUILD_DIR}/src
    ${MESA3D_DIR}/src/vulkan/runtime
    ${MESA3D_DIR}/src/vulkan/util
    ${MESA3D_DIR}/src/vulkan/wsi
    ${MESA3D_DIR}/src/compiler
    ${MESA3D_DIR}/src/util
    ${MESA3D_DIR}/src
    ${MESA3D_DIR}/include
    ${MESA3D_DIR}/include/vulkan
    ${MESA3D_DIR}/include/drm-uapi
    ${MESA3D_DIR}/subprojects/libdrm
    ${MESA3D_DIR}/subprojects/libdrm/include/drm
    ${FRAMEWORKS_NATIVE_DIR}/libs/nativewindow/include
    ${FRAMEWORKS_NATIVE_DIR}/libs/nativebase/include
    ${FRAMEWORKS_NATIVE_DIR}/libs/arect/include
    ${MESA3D_DIR}/include/android_stub
    ${GFXSTREAM_GUEST}/OpenglSystemCommon
    ${GFXSTREAM_GUEST}/renderControl_enc
    ${GFXSTREAM_GUEST}/OpenglCodecCommon
    ${GFXSTREAM_GUEST}/OpenglCodecCommon/include
    ${GFXSTREAM_GUEST}/include
    ${GFXSTREAM_DIR}/include/opengl
    ${GFXSTREAM_GUEST}/android-emu
    ${GFXSTREAM_GUEST}/gralloc_cb/include
    ${GFXSTREAM_GUEST}/qemupipe/include
    ${GFXSTREAM_GUEST}/qemupipe/include-types
    ${SCRATCH_DIR}/minigbm
    ${LIBBASE_DIR}/include
    ${SYSTEM_CORE_DIR}/libcutils/include
    ${SYSTEM_CORE_DIR}/libutils/include
    ${SYSTEM_CORE_DIR}/libsystem/include
    ${SYSTEM_LOGGING_DIR}/liblog/include
)

target_compile_definitions(vulkan.ranchu PRIVATE
    LOG_TAG="goldfish_vulkan"
    VK_NO_PROTOTYPES
    VK_USE_PLATFORM_ANDROID_KHR
    HAVE_ANDROID_PLATFORM
    _GNU_SOURCE
    HAVE_PTHREAD
    HAVE_LIBDRM
    HAVE_ENDIAN_H
    HAVE_LINUX_FUTEX_H
    HAVE_STRUCT_TIMESPEC
    HAVE_DLFCN_H
    HAVE_FUNC_ATTRIBUTE_VISIBILITY
    HAVE_FUNC_ATTRIBUTE_PACKED
    HAVE_FUNC_ATTRIBUTE_WEAK
    HAVE___BUILTIN_EXPECT
    HAVE___BUILTIN_UNREACHABLE
    HAVE___BUILTIN_CLZ
    HAVE___BUILTIN_CLZLL
    HAVE___BUILTIN_FFS
    HAVE___BUILTIN_FFSLL
    HAVE___BUILTIN_POPCOUNT
    HAVE___BUILTIN_POPCOUNTLL
    MAJOR_IN_SYSMACROS
    USE_GCC_ATOMIC_BUILTINS
)

target_compile_options(vulkan.ranchu PRIVATE
    -fvisibility=hidden
    -fstrict-aliasing
    -ffunction-sections
    -fdata-sections
    -fno-emulated-tls
    -Wno-missing-field-initializers
    -Wno-unused-function
    -Wno-unused-parameter
    -Wno-cast-calling-convention
)

set_target_properties(vulkan.ranchu PROPERTIES
    PREFIX ""
    OUTPUT_NAME "vulkan.ranchu"
)

# Mesa's static libraries, from its Meson build. They reference each other, so they are
# linked as one group.
set(MESA_STATIC_LIBS
    ${MESA_BUILD_DIR}/src/vulkan/runtime/libvulkan_lite_runtime.a
    ${MESA_BUILD_DIR}/src/vulkan/runtime/libvulkan_lite_instance.a
    ${MESA_BUILD_DIR}/src/vulkan/wsi/libvulkan_wsi.a
    ${MESA_BUILD_DIR}/src/vulkan/util/libvulkan_util.a
    ${MESA_BUILD_DIR}/src/compiler/libcompiler.a
    ${MESA_BUILD_DIR}/src/util/u_gralloc/lib_mesa_u_gralloc.a
    ${MESA_BUILD_DIR}/src/util/libmesa_util.a
    ${MESA_BUILD_DIR}/src/util/libxmlconfig.a
    ${MESA_BUILD_DIR}/src/util/libmesa_util_sse41.a
    ${MESA_BUILD_DIR}/src/util/blake3/libblake3.a
    ${MESA_BUILD_DIR}/src/c11/impl/libmesa_util_c11.a
    ${MESA_BUILD_DIR}/src/gfxstream/aemu/libaemu.a
)

# Every symbol must resolve now: the platform loads the driver with immediate binding, so
# one that does not resolve makes it fail to load on the device. The platform libraries
# the NDK lacks are linked against Mesa's stubs, libnativewindow against a stub with the
# vendor-only call, and libdrm against the one Meson builds.
target_link_options(vulkan.ranchu PRIVATE
    -Wl,--no-undefined
    # Export only the HAL module the Android Vulkan loader looks up, as Mesa does.
    -Wl,--version-script=${MESA3D_DIR}/src/vulkan/vulkan-android.sym
    -Wl,-Bsymbolic
    -Wl,--gc-sections
)

target_link_libraries(vulkan.ranchu PRIVATE
    -Wl,--start-group ${MESA_STATIC_LIBS} -Wl,--end-group
    ${MESA_BUILD_DIR}/subprojects/libdrm/libdrm.so
    ${MESA_BUILD_DIR}/src/android_stub/libcutils.so
    ${MESA_BUILD_DIR}/src/android_stub/libhardware.so
    ${MESA_BUILD_DIR}/src/android_stub/libsync.so
    nativewindow_stub
    ${MESA_BUILD_DIR}/src/android_stub/liblog.so
    z
    m
    dl
)
