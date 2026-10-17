#!/usr/bin/env bash
# Compiles the generated Java classes with the vector test and runs it.
#
#   dsa/protocol/java/test.sh
#
# PROTOBUF_JAVA_JAR names the protobuf runtime jar (default: Debian's
# /usr/share/java/protobuf.jar, from the libprotobuf-java package). The full runtime
# includes the lite classes the generated code uses. Output goes to java/build/.
set -euo pipefail

java_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
protocol_dir="$(dirname "$java_dir")"
jar="${PROTOBUF_JAVA_JAR:-/usr/share/java/protobuf.jar}"
out="$java_dir/build"

rm -rf "$out"
mkdir -p "$out"
javac -encoding UTF-8 -cp "$jar" -d "$out" \
    $(find "$java_dir/src/main/java" "$java_dir/src/test/java" -name '*.java')
java -cp "$out:$jar" io.github.thisisthepy.dsa.control.v1.VectorsTest "$protocol_dir"
