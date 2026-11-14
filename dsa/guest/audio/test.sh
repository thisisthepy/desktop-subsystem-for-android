#!/usr/bin/env bash
# Builds and runs the audio module's tests on the build machine: the app outputs and their
# volume gain, the output and input streams' commands and pacing, the shared queues, the
# bytes waiting for the host's socket, the queue of the host's recorded sound, the input
# stream's connection to a fake host, and the audio port's messages through the generated
# codec.
#
#   dsa/guest/audio/test.sh
#
# Needs a C++17 compiler on Linux and the protobuf-lite headers and library of the same
# version as the protoc that generated dsa/protocol/cpp/generated (Debian: libprotobuf-dev).
# PROTOBUF_PREFIX points at another installation root (with include/ and lib/). Output goes
# to the ignored build/ folder beside this script.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
protocol="$(cd "$here/../../protocol" && pwd)"
runner="$(cd "$here/../hwcomposer/tests" && pwd)"
out="$here/build/test"
cxx="${CXX:-c++}"
flags=(-std=c++17 -Wall -Wextra -Werror -I"$here/src" -I"$runner")
protobuf=()
if [[ -n "${PROTOBUF_PREFIX:-}" ]]; then
    protobuf+=(-I"$PROTOBUF_PREFIX/include" -L"$PROTOBUF_PREFIX/lib" -Wl,-rpath,"$PROTOBUF_PREFIX/lib")
fi

rm -rf "$out"
mkdir -p "$out"

"$cxx" "${flags[@]}" "$here/src/output_stream_logic.cpp" "$here/tests/output_stream_logic_test.cpp" \
    -o "$out/output_stream_logic_test"
"$out/output_stream_logic_test"

"$cxx" "${flags[@]}" "$here/src/app_outputs.cpp" "$here/tests/app_outputs_test.cpp" \
    -o "$out/app_outputs_test"
"$out/app_outputs_test"

"$cxx" "${flags[@]}" "$here/src/outbound.cpp" "$here/tests/outbound_test.cpp" -o "$out/outbound_test"
"$out/outbound_test"

"$cxx" "${flags[@]}" "$here/src/shared_queue.cpp" "$here/tests/shared_queue_test.cpp" -pthread \
    -o "$out/shared_queue_test"
"$out/shared_queue_test"

"$cxx" "${flags[@]}" "$here/src/input_stream_logic.cpp" "$here/tests/input_stream_logic_test.cpp" \
    -o "$out/input_stream_logic_test"
"$out/input_stream_logic_test"

"$cxx" "${flags[@]}" "$here/src/capture_queue.cpp" "$here/tests/capture_queue_test.cpp" -pthread \
    -o "$out/capture_queue_test"
"$out/capture_queue_test"

# The generated codec is not ours to warn about.
"$cxx" -std=c++17 -I"$protocol/cpp/generated" "${protobuf[@]}" -c \
    "$protocol/cpp/generated/control.pb.cc" -o "$out/control.pb.o"
"$cxx" "${flags[@]}" -I"$protocol/cpp/generated" "${protobuf[@]}" \
    "$here/src/audio_messages.cpp" "$here/tests/audio_messages_test.cpp" "$out/control.pb.o" \
    -lprotobuf-lite -pthread -o "$out/audio_messages_test"
"$out/audio_messages_test"

"$cxx" "${flags[@]}" -I"$protocol/cpp/generated" "${protobuf[@]}" \
    "$here/src/audio_messages.cpp" "$here/src/socket_io.cpp" "$here/src/capture_queue.cpp" \
    "$here/src/input_link.cpp" "$here/tests/input_link_test.cpp" "$out/control.pb.o" \
    -lprotobuf-lite -pthread -o "$out/input_link_test"
"$out/input_link_test"
