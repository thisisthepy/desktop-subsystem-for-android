# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0
#
# dsa-hwcomposer for x86_64 Android, built with the NDK by build.sh, which fetches the
# pinned sources into SCRATCH_DIR and generates the binder code into AIDL_GEN_DIR first.
#
# The binary links nothing but the platform's C libraries (libc, libbinder_ndk, liblog): C++
# is the NDK's, linked in statically, because a C++ symbol built against the NDK's libc++ is
# not the one the platform's own C++ libraries export.

cmake_minimum_required(VERSION 3.22)
project(dsa_hwcomposer LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED SCRATCH_DIR OR NOT DEFINED AIDL_GEN_DIR)
    message(FATAL_ERROR "run build.sh, which sets SCRATCH_DIR and AIDL_GEN_DIR")
endif()

get_filename_component(REPO_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
set(GFXSTREAM_GUEST "${SCRATCH_DIR}/gfxstream/guest")
set(MESA_GUEST "${SCRATCH_DIR}/mesa3d/src/gfxstream/guest")
set(BINDER_NDK "${SCRATCH_DIR}/binder_ndk/libs/binder/ndk")
set(PROTOCOL_CPP "${REPO_DIR}/dsa/protocol/cpp/generated")

# protobuf-lite 3.21.12, for the generated control channel codec.
set(protobuf_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_PROTOC_BINARIES OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_LIBPROTOC OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(protobuf_WITH_ZLIB OFF CACHE BOOL "" FORCE)
set(protobuf_INSTALL OFF CACHE BOOL "" FORCE)
set(protobuf_DISABLE_RTTI ON CACHE BOOL "" FORCE)
add_subdirectory("${SCRATCH_DIR}/protobuf-21.12" protobuf EXCLUDE_FROM_ALL)

# A link-time libbinder_ndk.so: the NDK's copy of the library leaves out the platform calls
# a HAL service makes (registering a service, the thread pool), and the linker takes only
# one library of that name. So the stand-in defines every function the NDK's copy does, plus
# those in binder_ndk_platform_stub.c. Only the names matter: on the device the binary loads
# the platform's own libbinder_ndk.so.
find_library(NDK_BINDER binder_ndk REQUIRED)
execute_process(
    COMMAND "${CMAKE_NM}" -D --defined-only --format=just-symbols "${NDK_BINDER}"
    OUTPUT_VARIABLE NDK_BINDER_SYMBOLS
    RESULT_VARIABLE NDK_BINDER_NM_RESULT)
if(NOT NDK_BINDER_NM_RESULT EQUAL 0 OR NDK_BINDER_SYMBOLS STREQUAL "")
    message(FATAL_ERROR "cannot list the symbols of ${NDK_BINDER}")
endif()
file(READ binder_ndk_platform_stub.c BINDER_STUB_SOURCE)
string(REGEX MATCHALL "[A-Za-z_][A-Za-z0-9_]*\\(" PLATFORM_CALLS "${BINDER_STUB_SOURCE}")
string(REPLACE "\n" ";" NDK_BINDER_SYMBOLS "${NDK_BINDER_SYMBOLS}")
set(BINDER_STUB "${CMAKE_CURRENT_BINARY_DIR}/binder_ndk_ndk_stub.c")
set(BINDER_STUB_TEXT "// Generated from ${NDK_BINDER}\n")
foreach(symbol IN LISTS NDK_BINDER_SYMBOLS)
    string(REGEX REPLACE "@.*$" "" symbol "${symbol}")
    string(STRIP "${symbol}" symbol)
    if(symbol STREQUAL "" OR "${symbol}(" IN_LIST PLATFORM_CALLS)
        continue()
    endif()
    string(APPEND BINDER_STUB_TEXT "void ${symbol}(void) {}\n")
endforeach()
file(WRITE "${BINDER_STUB}" "${BINDER_STUB_TEXT}")
add_library(binder_ndk_stub SHARED binder_ndk_platform_stub.c "${BINDER_STUB}")
set_target_properties(binder_ndk_stub PROPERTIES
    OUTPUT_NAME "binder_ndk"
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/platform-stubs")
target_compile_options(binder_ndk_stub PRIVATE -Wno-builtin-requires-header -Wno-incompatible-library-redeclaration)

file(GLOB_RECURSE AIDL_SOURCES CONFIGURE_DEPENDS "${AIDL_GEN_DIR}/src/*.cpp")

add_executable(dsa-hwcomposer
    src/main.cpp
    src/channel_client.cpp
    src/channel_messages.cpp
    src/compose_request.cpp
    src/composer.cpp
    src/composer_client.cpp
    src/display.cpp
    src/frame_pipeline.cpp
    src/host_renderer.cpp
    src/host_stream.cpp
    src/resize_trace.cpp
    src/task_composer.cpp
    src/task_layout.cpp
    src/vsync_thread.cpp
    ${AIDL_SOURCES}
    ${PROTOCOL_CPP}/control.pb.cc
    # renderControl, the host renderer's control protocol.
    ${GFXSTREAM_GUEST}/renderControl_enc/renderControl_client_context.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/renderControl_enc.cpp
    ${GFXSTREAM_GUEST}/renderControl_enc/ExtendedRenderControl.cpp
    ${GFXSTREAM_GUEST}/OpenglCodecCommon/ChecksumCalculator.cpp
    ${GFXSTREAM_GUEST}/OpenglCodecCommon/glUtils.cpp
    ${GFXSTREAM_GUEST}/android-emu/aemu/base/Tracing.cpp
)

target_include_directories(dsa-hwcomposer PRIVATE
    src
    ${REPO_DIR}/dsa/graphics/include
)
# Fetched and generated headers, whose warnings are not ours to fix.
target_include_directories(dsa-hwcomposer SYSTEM PRIVATE
    ${AIDL_GEN_DIR}/include
    ${BINDER_NDK}/include_cpp
    ${BINDER_NDK}/include_ndk
    ${BINDER_NDK}/include_platform
    ${PROTOCOL_CPP}
    ${SCRATCH_DIR}/protobuf-21.12/src
    ${SCRATCH_DIR}/minigbm
    ${SCRATCH_DIR}/system_core/libcutils/include
    ${SCRATCH_DIR}/system_logging/liblog/include
    ${GFXSTREAM_GUEST}/renderControl_enc
    ${GFXSTREAM_GUEST}/OpenglCodecCommon
    ${GFXSTREAM_GUEST}/OpenglCodecCommon/include
    ${GFXSTREAM_GUEST}/android-emu
    ${GFXSTREAM_GUEST}/include
    ${SCRATCH_DIR}/gfxstream/include/opengl
    ${MESA_GUEST}/iostream/include
)

# Built as the vendor half of the system/vendor split, as the platform builds a vendor HAL:
# the binder objects it makes carry vendor stability, and those of its VINTF interfaces
# (the composer, the channel listener) VINTF stability, without which SurfaceFlinger and
# dsa-channeld would refuse to talk to them.
target_compile_definitions(dsa-hwcomposer PRIVATE
    LOG_TAG="dsa-hwcomposer"
    __ANDROID_VENDOR__
    BINDER_STABILITY_SUPPORT
)
target_compile_options(dsa-hwcomposer PRIVATE
    -fvisibility=hidden
    -ffunction-sections
    -fdata-sections
    -Wno-unused-parameter
    -Wno-deprecated-declarations
)

# DSA's own sources build without warnings; the fetched and generated ones are not ours to fix.
file(GLOB DSA_HWC_SOURCES CONFIGURE_DEPENDS src/*.cpp)
set_source_files_properties(${DSA_HWC_SOURCES} PROPERTIES COMPILE_OPTIONS "-Wall;-Wextra;-Werror")

target_link_options(dsa-hwcomposer PRIVATE -Wl,--gc-sections -Wl,--as-needed -s)
target_link_libraries(dsa-hwcomposer PRIVATE
    libprotobuf-lite
    binder_ndk_stub
    log
)
