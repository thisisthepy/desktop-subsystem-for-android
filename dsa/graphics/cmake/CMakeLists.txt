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
