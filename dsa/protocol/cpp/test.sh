#!/usr/bin/env bash
# Builds the generated C++ sources with the vector test and runs it.
#
#   dsa/protocol/cpp/test.sh
#
# Needs a C++17 compiler and the protobuf-lite headers and library of the same version
# as the protoc that generated the sources (Debian: libprotobuf-dev). PROTOBUF_PREFIX
# points at another installation root (with include/ and lib/). Output goes to cpp/build/.
set -euo pipefail

cpp_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
protocol_dir="$(dirname "$cpp_dir")"
out="$cpp_dir/build"
flags=()
if [[ -n "${PROTOBUF_PREFIX:-}" ]]; then
    flags+=(-I"$PROTOBUF_PREFIX/include" -L"$PROTOBUF_PREFIX/lib" -Wl,-rpath,"$PROTOBUF_PREFIX/lib")
fi

rm -rf "$out"
mkdir -p "$out"
"${CXX:-c++}" -std=c++17 -Wall -Werror -I"$cpp_dir/generated" "${flags[@]}" \
    "$cpp_dir/generated/control.pb.cc" "$cpp_dir/vectors_test.cc" \
    -lprotobuf-lite -pthread -o "$out/vectors_test"
"$out/vectors_test" "$protocol_dir"
