#!/usr/bin/env bash
# Builds and runs the codec HAL's tests on the build machine: its video port messages against
# the protocol vectors, and the copy of a decoded picture into a buffer's planes.
#
#   dsa/guest/codec2/test.sh
#
# Needs a C++17 compiler and the protobuf-lite headers and library of the same version as
# the protoc that generated dsa/protocol/cpp/generated (Debian: libprotobuf-dev).
# PROTOBUF_PREFIX points at another installation root (with include/ and lib/). Output goes
# to the ignored build/ folder beside this script.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
protocol="$(cd "$here/../../protocol" && pwd)"
out="$here/build/test"
cxx="${CXX:-c++}"
flags=(-std=c++17 -Wall -Wextra -Werror -I"$here/src")
protobuf=()
if [[ -n "${PROTOBUF_PREFIX:-}" ]]; then
    protobuf+=(-I"$PROTOBUF_PREFIX/include" -L"$PROTOBUF_PREFIX/lib" -Wl,-rpath,"$PROTOBUF_PREFIX/lib")
fi

rm -rf "$out"
mkdir -p "$out"

# The generated codec is not ours to warn about.
"$cxx" -std=c++17 -I"$protocol/cpp/generated" "${protobuf[@]}" -c \
    "$protocol/cpp/generated/control.pb.cc" -o "$out/control.pb.o"
"$cxx" "${flags[@]}" -I"$here/tests" -I"$here/../hwcomposer/tests" -I"$protocol/cpp/generated" "${protobuf[@]}" \
    "$here/src/video_messages.cpp" "$here/src/picture_copy.cpp" "$here/src/output_buffer.cpp" \
    "$here/src/config_rules.cpp" "$here/src/output_gate.cpp" "$here/src/send_queue.cpp" \
    "$here/src/arrival_queue.cpp" "$here/src/picture_stats.cpp" \
    "$here/tests/codec2_test.cpp" "$out/control.pb.o" \
    -lprotobuf-lite -pthread -o "$out/codec2_test"
"$out/codec2_test" "$protocol"
