#!/usr/bin/env bash
# Runs Digitalis' host tests (berberis_arm64_host_tests) from a tree build.sh built, and
# keeps their XML report and full output in <report dir>.
#
#   host-tests.sh <tree> <report dir>
#
# The binary links the translator statically and runs translated ARM64 code on this
# x86_64 runner, so it tests the same translator the package carries.
set -euo pipefail

top="$(cd "$1" && pwd)"
mkdir -p "$2"
report="$(cd "$2" && pwd)"
binary="$(find "$top/out/host" -type f -name berberis_arm64_host_tests -perm -u+x -print -quit)"
if [[ -z "$binary" ]]; then
  echo "the build made no berberis_arm64_host_tests" >&2
  exit 1
fi
echo "running $binary"
cd "$(dirname "$binary")"
status=0
timeout 75m "$binary" --gtest_color=no --gtest_output="xml:$report/berberis_arm64_host_tests.xml" \
  > "$report/berberis_arm64_host_tests.txt" 2>&1 || status=$?
grep -E "^\[  (PASSED|FAILED|SKIPPED) |^\[==========\]|^\[  FAILED  \]" "$report/berberis_arm64_host_tests.txt" | tail -n 60 || true
if (( status == 124 )); then
  echo "the host tests did not finish in 75 minutes" >&2
fi
exit "$status"
