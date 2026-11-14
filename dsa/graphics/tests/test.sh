#!/usr/bin/env bash
# Builds and runs the guest graphics transport's tests on the build machine, against the
# patched upstream trees dsa/graphics/fetch.sh placed in the scratch directory.
#
#   dsa/graphics/tests/test.sh [scratch-dir]
#
# Needs a C++17 compiler. Output goes to the scratch directory.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
scratch="$(cd "${1:-$root/.scratch}" && pwd)"
out="$scratch/graphics-tests"
cxx="${CXX:-c++}"

rm -rf "$out"
mkdir -p "$out"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/shadow_blocks_test.cpp" -o "$out/shadow_blocks_test"
"$out/shadow_blocks_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/shadow_record_test.cpp" -o "$out/shadow_record_test"
"$out/shadow_record_test"

# The guard on what host-visible memory adds to a resized window's wait for a frame.
"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/readback_latency_test.cpp" -o "$out/readback_latency_test"
"$out/readback_latency_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/shadow_memory_test.cpp" -o "$out/shadow_memory_test"
"$out/shadow_memory_test"

# The memory types the guest reports without shared memory, and which of them are read back
# when the app's work finishes (guest/mesa 0012).
"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/memory_types_test.cpp" -o "$out/memory_types_test"
"$out/memory_types_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/lost_stream_test.cpp" -o "$out/lost_stream_test"
"$out/lost_stream_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/cpu_readback_test.cpp" -o "$out/cpu_readback_test"
"$out/cpu_readback_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/cpu_upload_test.cpp" -o "$out/cpu_upload_test"
"$out/cpu_upload_test"

# How long a process keeps a gralloc buffer's host ColorBuffer open (guest/mesa 0013).
"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/color_buffer_refs_test.cpp" -o "$out/color_buffer_refs_test"
"$out/color_buffer_refs_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/spare_pages_test.cpp" -o "$out/spare_pages_test"
"$out/spare_pages_test"

# When a render thread's Vulkan reply buffer goes back to its normal size (patch 0028).
"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/reply_buffer_test.cpp" -o "$out/reply_buffer_test"
"$out/reply_buffer_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../host" \
    "$here/frame_crop_test.cpp" -o "$out/frame_crop_test"
"$out/frame_crop_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../host" \
    "$here/copy_wait_test.cpp" -o "$out/copy_wait_test"
"$out/copy_wait_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/copy_bands_test.cpp" -o "$out/copy_bands_test"
"$out/copy_bands_test"

"$cxx" -std=c++17 -O2 -Wall -Wextra -Werror -I"$here/../include" \
    "$here/device_remake_test.cpp" -o "$out/device_remake_test"
"$out/device_remake_test"

# The upstream headers are not ours to warn about.
upstream=(
    -isystem "$scratch/gfxstream/guest/OpenglSystemCommon"
    -isystem "$scratch/mesa3d/src/gfxstream/guest/iostream/include"
    -isystem "$scratch/system_core/libcutils/include"
    -isystem "$scratch/system_logging/liblog/include"
)

"$cxx" -std=c++17 "${upstream[@]}" -I"$here/../include" -c \
    "$scratch/gfxstream/guest/OpenglSystemCommon/HvSocketStream.cpp" -o "$out/HvSocketStream.o"
"$cxx" -std=c++17 -Wall -Wextra -Werror "${upstream[@]}" \
    "$here/hvsocket_stream_test.cpp" "$out/HvSocketStream.o" -o "$out/hvsocket_stream_test"
"$out/hvsocket_stream_test"
