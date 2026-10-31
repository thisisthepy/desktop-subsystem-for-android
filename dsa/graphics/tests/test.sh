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

# The upstream headers are not ours to warn about.
upstream=(
    -isystem "$scratch/gfxstream/guest/OpenglSystemCommon"
    -isystem "$scratch/mesa3d/src/gfxstream/guest/iostream/include"
    -isystem "$scratch/system_core/libcutils/include"
    -isystem "$scratch/system_logging/liblog/include"
)

"$cxx" -std=c++17 "${upstream[@]}" -c \
    "$scratch/gfxstream/guest/OpenglSystemCommon/HvSocketStream.cpp" -o "$out/HvSocketStream.o"
"$cxx" -std=c++17 -Wall -Wextra -Werror "${upstream[@]}" \
    "$here/hvsocket_stream_test.cpp" "$out/HvSocketStream.o" -o "$out/hvsocket_stream_test"
"$out/hvsocket_stream_test"
