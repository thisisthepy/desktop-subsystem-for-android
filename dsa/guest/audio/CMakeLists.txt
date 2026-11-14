# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0
#
# dsa-audio for x86_64 or arm64 Android, built with the NDK by build.sh, which fetches the pinned
# sources into SCRATCH_DIR and generates the binder code into AIDL_GEN_DIR first.
#
# The binary links nothing but the platform's C libraries (libc, libbinder_ndk, liblog): C++
# is the NDK's, linked in statically, because a C++ symbol built against the NDK's libc++ is
# not the one the platform's own C++ libraries export.

cmake_minimum_required(VERSION 3.22)
project(dsa_audio LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED SCRATCH_DIR OR NOT DEFINED AIDL_GEN_DIR)
    message(FATAL_ERROR "run build.sh, which sets SCRATCH_DIR and AIDL_GEN_DIR")
endif()

get_filename_component(REPO_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
set(BINDER_NDK "${SCRATCH_DIR}/binder_ndk/libs/binder/ndk")
set(PROTOCOL_CPP "${REPO_DIR}/dsa/protocol/cpp/generated")
# The hwcomposer's list of the platform calls a HAL service makes, which the NDK's copy of
# libbinder_ndk leaves out.
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

# A link-time libbinder_ndk.so: every function the NDK's copy defines, plus the platform
# calls in the stub. Only the names matter: on the device the binary loads the platform's
# own libbinder_ndk.so.
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

file(GLOB_RECURSE AIDL_SOURCES CONFIGURE_DEPENDS "${AIDL_GEN_DIR}/src/*.cpp")

add_executable(dsa-audio
    src/main.cpp
    src/module.cpp
    src/app_outputs.cpp
    src/stream_out.cpp
    src/stream_in.cpp
    src/input_stream_logic.cpp
    src/input_link.cpp
    src/capture_queue.cpp
    src/socket_io.cpp
    src/output_stream_logic.cpp
    src/shared_queue.cpp
    src/host_link.cpp
    src/outbound.cpp
    src/audio_messages.cpp
    ${AIDL_SOURCES}
    ${PROTOCOL_CPP}/control.pb.cc
)

target_include_directories(dsa-audio PRIVATE src)
# Fetched and generated headers, whose warnings are not ours to fix.
target_include_directories(dsa-audio SYSTEM PRIVATE
    ${AIDL_GEN_DIR}/include
    ${BINDER_NDK}/include_cpp
    ${BINDER_NDK}/include_ndk
    ${BINDER_NDK}/include_platform
    ${PROTOCOL_CPP}
    ${SCRATCH_DIR}/protobuf-21.12/src
)

# Built as the vendor half of the system/vendor split, as the platform builds a vendor HAL:
# the module and its streams carry VINTF stability, without which audioserver refuses them.
target_compile_definitions(dsa-audio PRIVATE
    __ANDROID_VENDOR__
    BINDER_STABILITY_SUPPORT
)
target_compile_options(dsa-audio PRIVATE
    -fvisibility=hidden
    -ffunction-sections
    -fdata-sections
    -Wno-unused-parameter
    -Wno-deprecated-declarations
)

# DSA's own sources build without warnings; the fetched and generated ones are not ours to fix.
file(GLOB DSA_AUDIO_SOURCES CONFIGURE_DEPENDS src/*.cpp)
set_source_files_properties(${DSA_AUDIO_SOURCES} PROPERTIES COMPILE_OPTIONS "-Wall;-Wextra;-Werror")

target_link_options(dsa-audio PRIVATE -Wl,--gc-sections -Wl,--as-needed -s)
target_link_libraries(dsa-audio PRIVATE
    libprotobuf-lite
    binder_ndk_stub
    log
)
