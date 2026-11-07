#!/usr/bin/env bash
# Checks that the Test workflow asks only for runners GitHub still hands out (a job asking
# for macos-14 was cancelled without starting), and that its weston installs retry with a
# time limit instead of stalling until the job's timeout.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test.yml"
if grep -Eq 'runs-on: macos-1[0-4]$' "$workflow"; then
  echo "test.yml asks for a retired macOS runner" >&2
  exit 1
fi
test "$(grep -c 'timeout 300 sudo DEBIAN_FRONTEND=noninteractive apt-get install' "$workflow")" -ge 2

# The macOS settings app's icon is checked size by size, as the package test does.
grep -Fq 'test -s "$RUNNER_TEMP/DSA.iconset/icon_$size.png"' "$workflow"
echo "test workflow runners: ok"
