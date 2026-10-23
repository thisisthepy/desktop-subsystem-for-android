# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0

cmake_minimum_required(VERSION 3.22)
project(host_gfxstream LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_C_STANDARD 11)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED SCRATCH_DIR)
    set(SCRATCH_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../../.scratch")
endif()
get_filename_component(SCRATCH_DIR "${SCRATCH_DIR}" ABSOLUTE)

set(AEMU_DIR "${SCRATCH_DIR}/aemu")
set(GFXSTREAM_DIR "${SCRATCH_DIR}/gfxstream")
set(ANGLE_DIR "${SCRATCH_DIR}/angle")

if(NOT EXISTS "${AEMU_DIR}/CMakeLists.txt")
    message(FATAL_ERROR "aemu repository not found at ${AEMU_DIR}")
endif()
if(NOT EXISTS "${GFXSTREAM_DIR}/CMakeLists.txt")
    message(FATAL_ERROR "gfxstream repository not found at ${GFXSTREAM_DIR}")
endif()
if(NOT EXISTS "${ANGLE_DIR}/include")
    message(FATAL_ERROR "angle include directory not found at ${ANGLE_DIR}/include")
endif()

if(WIN32)
    add_compile_definitions(
        UNICODE
        _UNICODE
        NOMINMAX
        EMUGL_BUILD
        VK_USE_PLATFORM_WIN32_KHR
        BUILDING_EMUGL_COMMON_SHARED
        _CRT_SECURE_NO_WARNINGS
        _CRT_NONSTDC_NO_DEPRECATE
    )
endif()

# AEMU base configuration for gfxstream
set(AEMU_COMMON_BUILD_CONFIG "gfxstream" CACHE STRING "" FORCE)
set(BUILD_STANDALONE ON CACHE BOOL "" FORCE)
set(ENABLE_VKCEREAL_TESTS OFF CACHE BOOL "" FORCE)

add_subdirectory("${AEMU_DIR}" aemu_build)

if(WIN32 AND TARGET aemu-base)
    target_link_libraries(aemu-base PRIVATE ws2_32 Shlwapi)
endif()

# Stub and interface libraries expected by gfxstream third-party/CMakeLists.txt
if(NOT TARGET flatbuffers)
    add_library(flatbuffers INTERFACE)
endif()

if(NOT TARGET libdrm_headers)
    add_library(libdrm_headers INTERFACE)
endif()

if(NOT TARGET gfxstream_vulkan_headers)
    add_library(gfxstream_vulkan_headers INTERFACE)
    target_include_directories(gfxstream_vulkan_headers INTERFACE
        "${GFXSTREAM_DIR}/common/vulkan/include"
    )
    target_compile_definitions(gfxstream_vulkan_headers INTERFACE
        VK_GFXSTREAM_STRUCTURE_TYPE_EXT
        VK_USE_PLATFORM_WIN32_KHR
    )
endif()

if(NOT TARGET gfxstream_egl_headers)
    add_library(gfxstream_egl_headers INTERFACE)
    target_include_directories(gfxstream_egl_headers INTERFACE
        "${ANGLE_DIR}/include"
        "${GFXSTREAM_DIR}/include"
    )
endif()

# Options to omit components DSA does not need
set(ENABLE_VKCEREAL_TESTS OFF CACHE BOOL "" FORCE)
set(BUILD_GRAPHICS_DETECTOR OFF CACHE BOOL "" FORCE)
set(ASTC_CPU_DECODING OFF CACHE BOOL "" FORCE)
set(GFXSTREAM_ENABLE_HOST_TRACING OFF CACHE BOOL "" FORCE)
set(USE_ANGLE_SHADER_PARSER OFF CACHE BOOL "" FORCE)
set(WITH_BENCHMARK OFF CACHE BOOL "" FORCE)
set(VIRGL_RENDERER_UNSTABLE_APIS ON CACHE BOOL "" FORCE)

add_subdirectory("${GFXSTREAM_DIR}" gfxstream_build EXCLUDE_FROM_ALL)

# Suppress warning escalation to error for compiler variations
if(TARGET gfxstream_backend_static)
    target_compile_options(gfxstream_backend_static PRIVATE -Wno-error)
endif()
if(TARGET gfxstream_backend)
    target_compile_options(gfxstream_backend PRIVATE -Wno-error)
endif()

# Staging distribution target
set(DIST_DIR "${CMAKE_BINARY_DIR}/dist" CACHE PATH "Distribution directory")

add_custom_target(dist_host_gfxstream ALL
    COMMAND ${CMAKE_COMMAND} -E make_directory "${DIST_DIR}/bin"
    COMMAND ${CMAKE_COMMAND} -E make_directory "${DIST_DIR}/lib"
    COMMAND ${CMAKE_COMMAND} -E make_directory "${DIST_DIR}/include/gfxstream"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        "$<TARGET_FILE:gfxstream_backend>"
        "${DIST_DIR}/bin/"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        "$<TARGET_LINKER_FILE:gfxstream_backend>"
        "${DIST_DIR}/lib/"
    COMMAND ${CMAKE_COMMAND} -E copy_if_different
        "${GFXSTREAM_DIR}/host/include/gfxstream/virtio-gpu-gfxstream-renderer.h"
        "${GFXSTREAM_DIR}/host/include/gfxstream/virtio-gpu-gfxstream-renderer-unstable.h"
        "${GFXSTREAM_DIR}/host/include/gfxstream/virtio-gpu-gfxstream-renderer-goldfish.h"
        "${DIST_DIR}/include/gfxstream/"
    DEPENDS gfxstream_backend
)
