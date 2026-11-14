#!/usr/bin/env bash
# Checks that the graphics CI compiles the patched guest transport with the Android NDK
# and CMake, and builds the host graphics renderer on Windows.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-graphics.yml"
grep -Fq 'cancel-in-progress: false' "$workflow"
grep -Fq 'setup-ndk' "$workflow"
grep -Fq 'dsa/graphics/fetch.sh' "$workflow"
grep -Fq 'dsa/graphics/build.sh' "$workflow"
grep -Fq 'guest-gfxstream-hvsocket' "$workflow"
grep -Fq 'libOpenglSystemCommon.so' "$workflow"
grep -Fq 'HvSocketStream' "$workflow"
grep -Fq 'vulkan.ranchu.so' "$workflow"
grep -Fq 'test "$exports" = "HMI"' "$workflow"
grep -Fq 'HMI' "$workflow"
grep -Fq 'Build the host graphics renderer (windows, ' "$workflow"
# ARM64 Windows hosts get their own renderer, built on an ARM64 runner (FR-27).
grep -Fq 'windows-11-arm' "$workflow"
grep -Fq 'host-gfxstream-windows-arm64' "$workflow"
grep -Fq 'dsa/graphics/host/build.ps1' "$workflow"
grep -Fq 'test_host_renderer.py' "$workflow"
grep -Fq 'host-gfxstream-windows' "$workflow"
grep -Fq 'gfxstream_backend.dll' "$workflow"
grep -Fq 'nfr5_unknown_vulkan_image_is_rejected_without_reaching_the_driver' "$workflow"
grep -Fq 'dsa/graphics/tests/test.sh' "$workflow"
# The Vulkan CTS subset stays valid and the test apps the PC checks use are built.
grep -Fq 'verify.py check-subset' "$workflow"
grep -Fq 'dsa/graphics/verify/build-test-apps.sh' "$workflow"
grep -Fq 'name: graphics-test-apps' "$workflow"
# The Linux host's renderer is built natively and hands frames over by read-back.
grep -Fq 'Build the host graphics renderer (linux, x64)' "$workflow"
grep -Fq 'dsa/graphics/host/build.sh' "$workflow"
grep -Fq 'dsa_gfx_set_display_pixels_callback' "$workflow"
# The same renderer for macOS, on Vulkan through MoltenVK.
grep -Fq 'Build the host graphics renderer (macos, ' "$workflow"
grep -Fq 'DSA_GRAPHICS_TARGET: host-macos' "$workflow"
grep -Fq 'host-gfxstream-macos' "$workflow"
grep -Fq 'libgfxstream_backend.dylib' "$workflow"
# On macOS the frames are copied on the GPU into the window manager's IOSurfaces.
grep -Fq '_dsa_gfx_set_display_iosurface_callbacks' "$workflow"
# Lavapipe's manifest is found by pattern, since Mesa 25 dropped the processor from its name.
grep -Fq 'ls /usr/share/vulkan/icd.d/lvp_icd*.json' "$workflow"
if grep -Fq 'VK_ICD_FILENAMES: /usr/share/vulkan/icd.d/lvp_icd.x86_64.json' "$workflow"; then
  echo "the Linux renderer test names a Lavapipe manifest that Mesa 25 no longer installs" >&2
  exit 1
fi
# On the Mac runner the GPU selection tests look for MoltenVK's device, not Lavapipe.
grep -Fq 'DSA_TEST_GPU_NAME=apple' "$workflow"
# The renderer process is killed while a guest draws, on every host, and the windows stay
# (NFR-5); the installer carries dsa-gfx.exe with its name.
grep -Fq 'cargo test -p dsa-host-windows --test renderer_process' "$workflow"
grep -Fq 'cargo test -p dsa-host-linux --test renderer_process' "$workflow"
grep -Fq 'cargo test -p dsa-host-macos --test renderer_process' "$workflow"
grep -Fq "'Graphics renderer for Android'" "$workflow"
