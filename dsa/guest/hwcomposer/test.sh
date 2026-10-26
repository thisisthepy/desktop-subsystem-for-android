#!/usr/bin/env bash
# Builds and runs the hwcomposer's tests on the build machine: the rules that split the
# display into per-task frames, and its channel messages against the protocol vectors.
#
#   dsa/guest/hwcomposer/test.sh
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

"$cxx" "${flags[@]}" -I"$here/tests" \
    "$here/src/task_layout.cpp" "$here/src/compose_request.cpp" "$here/tests/task_layout_test.cpp" \
    -o "$out/task_layout_test"
"$out/task_layout_test"

"$cxx" "${flags[@]}" -I"$here/tests" \
    "$here/src/resize_trace.cpp" "$here/tests/resize_trace_test.cpp" -o "$out/resize_trace_test"
"$out/resize_trace_test"

"$cxx" "${flags[@]}" -I"$here/tests" \
    "$here/src/vsync_thread.cpp" "$here/tests/vsync_thread_test.cpp" -pthread -o "$out/vsync_thread_test"
"$out/vsync_thread_test"

"$cxx" "${flags[@]}" -I"$here/tests" -I"$here/../../graphics/include" \
    "$here/tests/shared_color_buffer_test.cpp" -o "$out/shared_color_buffer_test"
"$out/shared_color_buffer_test"

# The generated codec is not ours to warn about.
"$cxx" -std=c++17 -I"$protocol/cpp/generated" "${protobuf[@]}" -c \
    "$protocol/cpp/generated/control.pb.cc" -o "$out/control.pb.o"
"$cxx" "${flags[@]}" -I"$here/tests" -I"$protocol/cpp/generated" "${protobuf[@]}" \
    "$here/src/task_layout.cpp" "$here/src/channel_messages.cpp" \
    "$here/tests/channel_messages_test.cpp" "$out/control.pb.o" \
    -lprotobuf-lite -pthread -o "$out/channel_messages_test"
"$out/channel_messages_test" "$protocol"
