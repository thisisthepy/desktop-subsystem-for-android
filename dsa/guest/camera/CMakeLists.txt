# Copyright 2026 The DSA Authors
# SPDX-License-Identifier: Apache-2.0
#
# dsa-camera for x86_64 or arm64 Android, built with the NDK by build.sh, which fetches the pinned
# sources into SCRATCH_DIR and generates the binder code into AIDL_GEN_DIR first.
#
# It builds the platform's camera metadata code from the fetched sources, so its metadata
# buffers are exactly the platform's; shim/ stands in for the two platform headers that code
# names. Like the hwcomposer, the binary links nothing but the platform's C libraries: C++ is
# the NDK's, linked in statically.

cmake_minimum_required(VERSION 3.22)
project(dsa_camera LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

if(NOT DEFINED SCRATCH_DIR OR NOT DEFINED AIDL_GEN_DIR)
    message(FATAL_ERROR "run build.sh, which sets SCRATCH_DIR and AIDL_GEN_DIR")
endif()

get_filename_component(REPO_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
set(BINDER_NDK "${SCRATCH_DIR}/camera_native/libs/binder/ndk")
set(CAMERA_MEDIA "${SCRATCH_DIR}/camera_media")
set(PROTOCOL_CPP "${REPO_DIR}/dsa/protocol/cpp/generated")
set(PLATFORM_STUB "${REPO_DIR}/dsa/guest/hwcomposer/binder_ndk_platform_stub.c")
set(NATIVEWINDOW_PLATFORM_STUB "${CMAKE_CURRENT_SOURCE_DIR}/nativewindow_platform_stub.c")

# protobuf-lite 3.21.12, for the generated camera port codec.
set(protobuf_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_PROTOC_BINARIES OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_LIBPROTOC OFF CACHE BOOL "" FORCE)
set(protobuf_BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(protobuf_WITH_ZLIB OFF CACHE BOOL "" FORCE)
set(protobuf_INSTALL OFF CACHE BOOL "" FORCE)
set(protobuf_DISABLE_RTTI ON CACHE BOOL "" FORCE)
add_subdirectory("${SCRATCH_DIR}/protobuf-21.12" protobuf EXCLUDE_FROM_ALL)

# stub_library(<target> <ndk library> <platform stub> <output name>): a link-time library that
# defines every function the NDK's copy of the library does, plus the platform calls in the
# stub. Only the names matter: on the device the binary loads the platform's own library.
function(stub_library target ndk_name platform_stub output_name)
    find_library(NDK_${target} ${ndk_name} REQUIRED)
    execute_process(
        COMMAND "${CMAKE_NM}" -D --defined-only --format=just-symbols "${NDK_${target}}"
        OUTPUT_VARIABLE symbols
        RESULT_VARIABLE nm_result)
    if(NOT nm_result EQUAL 0 OR symbols STREQUAL "")
        message(FATAL_ERROR "cannot list the symbols of ${NDK_${target}}")
    endif()
    file(READ "${platform_stub}" stub_source)
    string(REGEX MATCHALL "[A-Za-z_][A-Za-z0-9_]*\\(" platform_calls "${stub_source}")
    string(REPLACE "\n" ";" symbols "${symbols}")
    set(generated "${CMAKE_CURRENT_BINARY_DIR}/${target}_ndk_stub.c")
    set(text "// Generated from ${NDK_${target}}\n")
    foreach(symbol IN LISTS symbols)
        string(REGEX REPLACE "@.*$" "" symbol "${symbol}")
        string(STRIP "${symbol}" symbol)
        if(symbol STREQUAL "" OR "${symbol}(" IN_LIST platform_calls)
            continue()
        endif()
        string(APPEND text "void ${symbol}(void) {}\n")
    endforeach()
    file(WRITE "${generated}" "${text}")
    add_library(${target} SHARED "${platform_stub}" "${generated}")
    set_target_properties(${target} PROPERTIES
        OUTPUT_NAME "${output_name}"
        LIBRARY_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/platform-stubs-${output_name}")
    target_compile_options(${target} PRIVATE
        -w -Wno-builtin-requires-header -Wno-incompatible-library-redeclaration)
endfunction()

stub_library(binder_ndk_stub binder_ndk "${PLATFORM_STUB}" binder_ndk)
stub_library(nativewindow_stub nativewindow "${NATIVEWINDOW_PLATFORM_STUB}" nativewindow)

file(GLOB_RECURSE AIDL_SOURCES CONFIGURE_DEPENDS "${AIDL_GEN_DIR}/src/*.cpp")

# The platform's camera metadata code, fetched; its warnings are not ours to fix. Its tag
# tables come in through camera_metadata.c itself.
set(PLATFORM_SOURCES ${CAMERA_MEDIA}/camera/src/camera_metadata.c)
set_source_files_properties(${PLATFORM_SOURCES} ${AIDL_SOURCES} PROPERTIES COMPILE_OPTIONS "-w")

add_executable(dsa-camera
    src/main.cpp
    src/provider.cpp
    src/device.cpp
    src/session.cpp
    src/characteristics.cpp
    src/metadata.cpp
    src/stream_config.cpp
    src/frame_copy.cpp
    src/graphic_buffer.cpp
    src/host_link.cpp
    src/camera_messages.cpp
    ${PLATFORM_SOURCES}
    ${AIDL_SOURCES}
    ${PROTOCOL_CPP}/control.pb.cc
)

target_include_directories(dsa-camera PRIVATE src ${REPO_DIR}/dsa/graphics/include)
# The shims go first, so they stand in for the platform's headers.
target_include_directories(dsa-camera SYSTEM BEFORE PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/shim)
target_include_directories(dsa-camera SYSTEM PRIVATE
    ${CAMERA_MEDIA}/camera/include
    ${CAMERA_MEDIA}/private/camera/include
    ${AIDL_GEN_DIR}/include
    ${BINDER_NDK}/include_cpp
    ${BINDER_NDK}/include_ndk
    ${BINDER_NDK}/include_platform
    ${PROTOCOL_CPP}
    ${SCRATCH_DIR}/protobuf-21.12/src
)

# Built as the vendor half of the system/vendor split, as the platform builds a vendor HAL:
# its binder objects carry vendor stability, and those of its VINTF interfaces VINTF
# stability, without which the camera service would refuse to talk to them.
target_compile_definitions(dsa-camera PRIVATE __ANDROID_VENDOR__ BINDER_STABILITY_SUPPORT)
target_compile_options(dsa-camera PRIVATE
    -fvisibility=hidden
    -ffunction-sections
    -fdata-sections
    -Wno-unused-parameter
    -Wno-deprecated-declarations
)

# DSA's own sources build without warnings.
file(GLOB DSA_CAMERA_SOURCES CONFIGURE_DEPENDS src/*.cpp)
set_source_files_properties(${DSA_CAMERA_SOURCES} PROPERTIES COMPILE_OPTIONS "-Wall;-Wextra;-Werror;-Wno-unused-parameter")

target_link_options(dsa-camera PRIVATE -Wl,--gc-sections -Wl,--as-needed -s)
target_link_libraries(dsa-camera PRIVATE
    libprotobuf-lite
    binder_ndk_stub
    nativewindow_stub
    log
)
