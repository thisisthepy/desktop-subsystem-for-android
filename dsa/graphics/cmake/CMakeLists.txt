# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.22)
project(guest_gfxstream LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED SCRATCH_DIR)
    set(SCRATCH_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../.scratch")
endif()

get_filename_component(SCRATCH_DIR "${SCRATCH_DIR}" ABSOLUTE)
set(GFXSTREAM_DIR "${SCRATCH_DIR}/gfxstream")
set(MESA3D_DIR "${SCRATCH_DIR}/mesa3d")
set(LIBHARDWARE_DIR "${SCRATCH_DIR}/libhardware")
set(LIBBASE_DIR "${SCRATCH_DIR}/libbase")
set(SYSTEM_CORE_DIR "${SCRATCH_DIR}/system_core")
set(SYSTEM_LOGGING_DIR "${SCRATCH_DIR}/system_logging")

add_compile_definitions(
    VK_GFXSTREAM_STRUCTURE_TYPE_EXT
    GFXSTREAM_ENABLE_GUEST_GOLDFISH=1
    ANDROID
    __ANDROID__
    ANDROID_API_LEVEL=34
    DETECT_OS_ANDROID=1
    HAVE_PTHREAD=1
    HAVE_STRUCT_TIMESPEC=1
    HAVE_LINUX_FUTEX_H=1
    HAVE_ENDIAN_H=1
)

add_compile_options(
    -Wno-inconsistent-missing-override
    -Wno-unused-parameter
    -Wno-unused-variable
)

set(SOURCES
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/FormatConversions.cpp
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/HostConnection.cpp
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/HvSocketStream.cpp
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/ProcessPipe.cpp
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/QemuPipeStream.cpp
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/ThreadInfo.cpp
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/VirtioGpuPipeStream.cpp
)

add_library(OpenglSystemCommon SHARED ${SOURCES})

target_include_directories(OpenglSystemCommon PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon
    ${GFXSTREAM_DIR}/guest/renderControl_enc
    ${GFXSTREAM_DIR}/guest/GLESv1_enc
    ${GFXSTREAM_DIR}/guest/GLESv2_enc
    ${GFXSTREAM_DIR}/guest/OpenglCodecCommon
    ${GFXSTREAM_DIR}/guest/OpenglCodecCommon/include
    ${GFXSTREAM_DIR}/guest/android-emu
    ${GFXSTREAM_DIR}/guest/android-emu/aemu/base
    ${GFXSTREAM_DIR}/guest/qemupipe/include
    ${GFXSTREAM_DIR}/guest/qemupipe/include-types
    ${MESA3D_DIR}/src/gfxstream/guest/iostream/include
    ${MESA3D_DIR}/src/gfxstream/guest/platform/include
    ${MESA3D_DIR}/src/gfxstream/guest/GoldfishAddressSpace/include
    ${MESA3D_DIR}/src/gfxstream/guest/android/include
    ${MESA3D_DIR}/src
    ${LIBHARDWARE_DIR}/include_all
    ${LIBHARDWARE_DIR}/include
    ${LIBBASE_DIR}/include
    ${SYSTEM_CORE_DIR}/libcutils/include
    ${SYSTEM_CORE_DIR}/libutils/include
    ${SYSTEM_CORE_DIR}/libsystem/include
    ${SYSTEM_CORE_DIR}/libnativebase/include
    ${SYSTEM_LOGGING_DIR}/liblog/include
)

# Stub shared libraries for Android platform and vendor dependencies.
# On the Android device these are provided by the vendor and system image.
function(add_stub_library name)
    set(stub_src "${CMAKE_CURRENT_BINARY_DIR}/stubs/${name}_stub.c")
    file(GENERATE OUTPUT "${stub_src}" CONTENT "void __stub_${name}() {}\n")
    add_library(${name} SHARED "${stub_src}")
    set_target_properties(${name} PROPERTIES
        OUTPUT_NAME "${name}"
    )
endfunction()

add_stub_library(base)
add_stub_library(cutils)
add_stub_library(drm)
add_stub_library(OpenglCodecCommon)
add_stub_library(_renderControl_enc)
add_stub_library(GLESv1_enc)
add_stub_library(GLESv2_enc)
add_stub_library(sync)
add_stub_library(nativewindow)
add_stub_library(hardware)
add_stub_library(utils)

set_target_properties(OpenglSystemCommon PROPERTIES
    OUTPUT_NAME "OpenglSystemCommon"
    LINK_FLAGS "-Wl,-z,undefs"
)

target_link_libraries(OpenglSystemCommon PRIVATE
    base
    cutils
    drm
    OpenglCodecCommon
    _renderControl_enc
    GLESv1_enc
    GLESv2_enc
    log
)

set(VULKAN_SOURCES
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan/gfxstream_vk_device.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan/gfxstream_vk_android.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan/gfxstream_vk_cmd.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan/gfxstream_vk_wsi_stub.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/AndroidHardwareBuffer.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/CommandBufferStagingStream.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/DescriptorSetVirtualization.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/HostVisibleMemoryVirtualization.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/Resources.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/Validation.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/VulkanHandleMapping.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/VulkanStreamGuest.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/gfxstream_vk_private.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/GfxStreamVulkanConnection.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/goldfish_vk_counting_guest.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/goldfish_vk_deepcopy_guest.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/goldfish_vk_extension_structs_guest.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/goldfish_vk_marshaling_guest.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/goldfish_vk_reserved_marshaling_guest.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/goldfish_vk_transform_guest.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/VkEncoder.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc/func_table.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/connection-manager/GfxStreamConnection.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/connection-manager/GfxStreamConnectionManager.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/connection-manager/QemuPipeStream.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/connection-manager/VirtioGpuPipeStream.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/android/ANativeWindowAndroid.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/android/GfxStreamGralloc.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/android/GrallocGoldfish.cpp
    ${MESA3D_DIR}/src/gfxstream/guest/android/GrallocMinigbm.cpp
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon/HvSocketStream.cpp
    ${GFXSTREAM_DIR}/guest/android-emu/aemu/base/files/Stream.cpp
    ${GFXSTREAM_DIR}/guest/android-emu/aemu/base/files/StreamSerializing.cpp
)

add_library(vulkan.ranchu SHARED ${VULKAN_SOURCES})

target_include_directories(vulkan.ranchu PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${MESA3D_DIR}/include
    ${MESA3D_DIR}/include/drm-uapi
    ${SCRATCH_DIR}/gen
    ${GFXSTREAM_DIR}/guest/OpenglSystemCommon
    ${GFXSTREAM_DIR}/guest/renderControl_enc
    ${GFXSTREAM_DIR}/guest/OpenglCodecCommon
    ${GFXSTREAM_DIR}/guest/OpenglCodecCommon/include
    ${GFXSTREAM_DIR}/guest/android-emu
    ${GFXSTREAM_DIR}/guest/android-emu/aemu/base
    ${GFXSTREAM_DIR}/guest/android-emu/aemu/base/files
    ${GFXSTREAM_DIR}/guest/gralloc_cb/include
    ${GFXSTREAM_DIR}/guest/qemupipe/include
    ${GFXSTREAM_DIR}/guest/qemupipe/include-types
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan
    ${MESA3D_DIR}/src/gfxstream/guest/vulkan_enc
    ${MESA3D_DIR}/src/gfxstream/guest/connection-manager
    ${MESA3D_DIR}/src/gfxstream/guest/android
    ${MESA3D_DIR}/src/gfxstream/guest/android/include
    ${MESA3D_DIR}/src/gfxstream/guest/iostream/include
    ${MESA3D_DIR}/src/gfxstream/guest/platform/include
    ${MESA3D_DIR}/src/gfxstream/guest/GoldfishAddressSpace/include
    ${MESA3D_DIR}/src/vulkan/util
    ${MESA3D_DIR}/src/vulkan/runtime
    ${MESA3D_DIR}/src/vulkan/wsi
    ${MESA3D_DIR}/src/util
    ${MESA3D_DIR}/src
    ${SCRATCH_DIR}/minigbm
    ${LIBHARDWARE_DIR}/include_all
    ${LIBHARDWARE_DIR}/include
    ${LIBBASE_DIR}/include
    ${SYSTEM_CORE_DIR}/libcutils/include
    ${SYSTEM_CORE_DIR}/libutils/include
    ${SYSTEM_CORE_DIR}/libsystem/include
    ${SYSTEM_CORE_DIR}/libnativebase/include
    ${SYSTEM_LOGGING_DIR}/liblog/include
)

target_compile_definitions(vulkan.ranchu PRIVATE
    LOG_TAG="goldfish_vulkan"
    VK_NO_PROTOTYPES
    VK_USE_PLATFORM_ANDROID_KHR
)

set_target_properties(vulkan.ranchu PROPERTIES
    OUTPUT_NAME "vulkan.ranchu"
    LINK_FLAGS "-Wl,-z,undefs -Wl,-Bsymbolic -Wl,--gc-sections"
)

target_link_libraries(vulkan.ranchu PRIVATE
    base
    cutils
    drm
    sync
    nativewindow
    hardware
    utils
    log
)
