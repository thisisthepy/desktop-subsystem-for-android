# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0
#
# dsa-power and dsa-health for x86_64 Android, built with the NDK by build.sh, which fetches
# the pinned sources into SCRATCH_DIR and generates the binder code into AIDL_GEN_DIR first.
#
# They are two programs because each HAL runs in its own SELinux domain (hal_power_default,
# hal_health_default). Each links nothing but the platform's C libraries (libc,
# libbinder_ndk, liblog): C++ is the NDK's, linked in statically, as for the hwcomposer.

cmake_minimum_required(VERSION 3.22)
project(dsa_power LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED SCRATCH_DIR OR NOT DEFINED AIDL_GEN_DIR)
    message(FATAL_ERROR "run build.sh, which sets SCRATCH_DIR and AIDL_GEN_DIR")
endif()

get_filename_component(REPO_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
set(BINDER_NDK "${SCRATCH_DIR}/binder_ndk/libs/binder/ndk")
set(PROTOCOL_CPP "${REPO_DIR}/dsa/protocol/cpp/generated")
# The platform calls a HAL service makes, which the NDK's libbinder_ndk.so leaves out; the
# hwcomposer's stand-in lists them.
set(PLATFORM_STUB "${REPO_DIR}/dsa/guest/hwcomposer/binder_ndk_platform_stub.c")

# protobuf-lite 3.21.12, for the generated control channel codec.
set(protobuf_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_PROTOC_BINARIES OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_LIBPROTOC OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(protobuf_WITH_ZLIB OFF CACHE BOOL "" FORCE)
set(protobuf_INSTALL OFF CACHE BOOL "" FORCE)
set(protobuf_DISABLE_RTTI ON CACHE BOOL "" FORCE)
add_subdirectory("${SCRATCH_DIR}/protobuf-21.12" protobuf EXCLUDE_FROM_ALL)

# A link-time libbinder_ndk.so that defines every function the NDK's copy does, plus the
# platform calls. Only the names matter: on the device the binaries load the platform's own.
find_library(NDK_BINDER binder_ndk REQUIRED)
execute_process(
    COMMAND "${CMAKE_NM}" -D --defined-only --format=just-symbols "${NDK_BINDER}"
    OUTPUT_VARIABLE NDK_BINDER_SYMBOLS
    RESULT_VARIABLE NDK_BINDER_NM_RESULT)
if(NOT NDK_BINDER_NM_RESULT EQUAL 0 OR NDK_BINDER_SYMBOLS STREQUAL "")
    message(FATAL_ERROR "cannot list the symbols of ${NDK_BINDER}")
endif()
file(READ "${PLATFORM_STUB}" BINDER_STUB_SOURCE)
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
add_library(binder_ndk_stub SHARED "${PLATFORM_STUB}" "${BINDER_STUB}")
set_target_properties(binder_ndk_stub PROPERTIES
    OUTPUT_NAME "binder_ndk"
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/platform-stubs")
target_compile_options(binder_ndk_stub PRIVATE -Wno-builtin-requires-header -Wno-incompatible-library-redeclaration)

# Each HAL's generated binder code, and the control channel's, which both use.
file(GLOB_RECURSE POWER_AIDL_SOURCES CONFIGURE_DEPENDS "${AIDL_GEN_DIR}/src/android/hardware/power/*.cpp")
file(GLOB_RECURSE HEALTH_AIDL_SOURCES CONFIGURE_DEPENDS "${AIDL_GEN_DIR}/src/android/hardware/health/*.cpp")
file(GLOB_RECURSE COMMON_AIDL_SOURCES CONFIGURE_DEPENDS "${AIDL_GEN_DIR}/src/android/hardware/common/*.cpp")
file(GLOB_RECURSE CHANNEL_AIDL_SOURCES CONFIGURE_DEPENDS "${AIDL_GEN_DIR}/src/io/*.cpp")

add_library(dsa_power_common STATIC
    src/power_messages.cpp
    src/battery.cpp
    src/power_modes.cpp
    ${CHANNEL_AIDL_SOURCES}
    ${PROTOCOL_CPP}/control.pb.cc
)

# The channel client logs, so each program builds it with its own log tag.
add_executable(dsa-power src/power_main.cpp src/power_service.cpp src/channel_client.cpp
    ${POWER_AIDL_SOURCES} ${COMMON_AIDL_SOURCES})
add_executable(dsa-health src/health_main.cpp src/health_service.cpp src/channel_client.cpp
    ${HEALTH_AIDL_SOURCES})
target_compile_definitions(dsa-power PRIVATE LOG_TAG="dsa-power")
target_compile_definitions(dsa-health PRIVATE LOG_TAG="dsa-health")

# DSA's own sources build without warnings; the fetched and generated ones are not ours to fix.
file(GLOB DSA_POWER_SOURCES CONFIGURE_DEPENDS src/*.cpp)
set_source_files_properties(${DSA_POWER_SOURCES} PROPERTIES COMPILE_OPTIONS "-Wall;-Wextra;-Werror")

foreach(target dsa_power_common dsa-power dsa-health)
    target_include_directories(${target} PRIVATE src)
    target_include_directories(${target} SYSTEM PRIVATE
        ${AIDL_GEN_DIR}/include
        ${BINDER_NDK}/include_cpp
        ${BINDER_NDK}/include_ndk
        ${BINDER_NDK}/include_platform
        ${PROTOCOL_CPP}
        ${SCRATCH_DIR}/protobuf-21.12/src
    )
    # Built as the vendor half of the system/vendor split, as the platform builds a vendor
    # HAL: the binder objects carry vendor stability, and those of the VINTF interfaces
    # (the HALs, the channel listener) VINTF stability, without which the framework and
    # dsa-channeld would refuse to talk to them.
    target_compile_definitions(${target} PRIVATE __ANDROID_VENDOR__ BINDER_STABILITY_SUPPORT)
    target_compile_options(${target} PRIVATE
        -fvisibility=hidden
        -ffunction-sections
        -fdata-sections
        -Wno-unused-parameter
        -Wno-deprecated-declarations
    )
endforeach()

foreach(target dsa-power dsa-health)
    target_link_options(${target} PRIVATE -Wl,--gc-sections -Wl,--as-needed -s)
    target_link_libraries(${target} PRIVATE dsa_power_common libprotobuf-lite binder_ndk_stub log)
endforeach()
