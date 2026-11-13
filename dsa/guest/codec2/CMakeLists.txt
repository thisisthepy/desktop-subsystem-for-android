# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0
#
# dsa-codec2 for x86_64 Android, built with the NDK by build.sh, which fetches the pinned
# sources into SCRATCH_DIR and generates the binder code into AIDL_GEN_DIR first.
#
# It builds the platform's own Codec 2.0 parameter helpers and HAL utilities from the fetched
# sources, so its components describe their parameters exactly as the platform's decoders
# do. The few platform libraries those name (libbase, libutils, libcutils) are not in the
# NDK; shim/ stands in for the parts they use. Like the hwcomposer, the binary links nothing
# but the platform's C libraries: C++ is the NDK's, linked in statically.

cmake_minimum_required(VERSION 3.22)
project(dsa_codec2 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED SCRATCH_DIR OR NOT DEFINED AIDL_GEN_DIR)
    message(FATAL_ERROR "run build.sh, which sets SCRATCH_DIR and AIDL_GEN_DIR")
endif()

get_filename_component(REPO_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
set(BINDER_NDK "${SCRATCH_DIR}/codec2_native/libs/binder/ndk")
set(PROTOCOL_CPP "${REPO_DIR}/dsa/protocol/cpp/generated")
set(AV "${SCRATCH_DIR}/codec2_av/media")
set(PLATFORM_STUB "${REPO_DIR}/dsa/guest/hwcomposer/binder_ndk_platform_stub.c")

# protobuf-lite 3.21.12, for the generated video port codec.
set(protobuf_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_PROTOC_BINARIES OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_LIBPROTOC OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(protobuf_WITH_ZLIB OFF CACHE BOOL "" FORCE)
set(protobuf_INSTALL OFF CACHE BOOL "" FORCE)
set(protobuf_DISABLE_RTTI ON CACHE BOOL "" FORCE)
add_subdirectory("${SCRATCH_DIR}/protobuf-21.12" protobuf EXCLUDE_FROM_ALL)

# A link-time libbinder_ndk.so that defines every function the NDK's copy does, plus the
# platform calls a HAL service makes. Only the names matter: on the device the binary loads
# the platform's own.
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

# The platform's Codec 2.0 sources, fetched; their warnings are not ours to fix.
set(PLATFORM_SOURCES
    ${AV}/codec2/vndk/util/C2InterfaceHelper.cpp
    ${AV}/codec2/vndk/util/C2InterfaceUtils.cpp
    ${AV}/codec2/vndk/util/C2ParamUtils.cpp
    ${AV}/codec2/vndk/util/C2Debug.cpp
    ${AV}/codec2/vndk/C2Config.cpp
    ${AV}/codec2/components/base/SimpleC2Interface.cpp
    ${AV}/codec2/hal/aidl/Configurable.cpp
    ${AV}/codec2/hal/aidl/ParamTypes.cpp
    ${AV}/module/foundation/MediaDefs.cpp
)
set_source_files_properties(${PLATFORM_SOURCES} ${AIDL_SOURCES} PROPERTIES COMPILE_OPTIONS "-w")

add_executable(dsa-codec2
    src/main.cpp
    src/store.cpp
    src/component.cpp
    src/decoder_interface.cpp
    src/host_link.cpp
    src/video_messages.cpp
    src/picture_copy.cpp
    src/output_buffer.cpp
    src/output_gate.cpp
    src/send_queue.cpp
    src/config_rules.cpp
    src/platform_support.cpp
    ${PLATFORM_SOURCES}
    ${AIDL_SOURCES}
    ${PROTOCOL_CPP}/control.pb.cc
)

target_include_directories(dsa-codec2 PRIVATE src)
# The shims go first, so they stand in for the platform libraries' headers.
target_include_directories(dsa-codec2 SYSTEM BEFORE PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/shim)
target_include_directories(dsa-codec2 SYSTEM PRIVATE
    ${AV}/codec2/core/include
    ${AV}/codec2/vndk/include
    ${AV}/codec2/vndk/internal
    ${AV}/codec2/components/base/include
    ${AV}/codec2/hal/common/include
    ${AV}/codec2/hal/aidl/include
    ${AV}/codec2/hal/aidl
    ${AV}/module/foundation/include
    ${AIDL_GEN_DIR}/include
    ${BINDER_NDK}/include_cpp
    ${BINDER_NDK}/include_ndk
    ${BINDER_NDK}/include_platform
    ${PROTOCOL_CPP}
    ${SCRATCH_DIR}/protobuf-21.12/src
)

# Built as the vendor half of the system/vendor split, as the platform builds a vendor HAL:
# its binder objects carry vendor stability, and those of its VINTF interfaces VINTF
# stability, without which the framework would refuse to talk to them.
target_compile_definitions(dsa-codec2 PRIVATE __ANDROID_VENDOR__ BINDER_STABILITY_SUPPORT)
target_compile_options(dsa-codec2 PRIVATE
    -fvisibility=hidden
    -ffunction-sections
    -fdata-sections
    -Wno-unused-parameter
    -Wno-deprecated-declarations
)

# DSA's own sources build without warnings.
file(GLOB DSA_CODEC2_SOURCES CONFIGURE_DEPENDS src/*.cpp)
set_source_files_properties(${DSA_CODEC2_SOURCES} PROPERTIES COMPILE_OPTIONS "-Wall;-Wextra;-Werror;-Wno-unused-parameter")

target_link_options(dsa-codec2 PRIVATE -Wl,--gc-sections -Wl,--as-needed -s)
target_link_libraries(dsa-codec2 PRIVATE
    libprotobuf-lite
    binder_ndk_stub
    nativewindow
    log
)
