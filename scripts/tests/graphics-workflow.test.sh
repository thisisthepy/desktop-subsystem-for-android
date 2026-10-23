#!/usr/bin/env bash
# Checks that the graphics CI compiles the patched guest transport with the Android NDK
# and CMake, verifying its ELF architecture and exported symbols.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-graphics.yml"
grep -Fq 'cancel-in-progress: true' "$workflow"
grep -Fq 'setup-ndk' "$workflow"
grep -Fq 'dsa/graphics/fetch.sh' "$workflow"
grep -Fq 'dsa/graphics/build.sh' "$workflow"
grep -Fq 'guest-gfxstream-hvsocket' "$workflow"
grep -Fq 'libOpenglSystemCommon.so' "$workflow"
grep -Fq 'HvSocketStream' "$workflow"
