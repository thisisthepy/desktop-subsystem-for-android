#!/usr/bin/env bash
# Checks that the graphics CI compiles the patched guest transport with the Android NDK
# and CMake, and builds the host graphics renderer on Windows.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-graphics.yml"
grep -Fq 'cancel-in-progress: true' "$workflow"
grep -Fq 'setup-ndk' "$workflow"
grep -Fq 'dsa/graphics/fetch.sh' "$workflow"
grep -Fq 'dsa/graphics/build.sh' "$workflow"
grep -Fq 'guest-gfxstream-hvsocket' "$workflow"
grep -Fq 'libOpenglSystemCommon.so' "$workflow"
grep -Fq 'HvSocketStream' "$workflow"
grep -Fq 'vulkan.ranchu.so' "$workflow"
grep -Fq 'test "$exports" = "HMI"' "$workflow"
grep -Fq 'HMI' "$workflow"
grep -Fq 'Build the host graphics renderer (windows)' "$workflow"
grep -Fq 'dsa/graphics/host/build.ps1' "$workflow"
grep -Fq 'test_host_renderer.py' "$workflow"
grep -Fq 'host-gfxstream-windows' "$workflow"
grep -Fq 'gfxstream_backend.dll' "$workflow"
