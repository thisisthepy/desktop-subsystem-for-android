#!/usr/bin/env bash
# Checks that the graphics CI compiles the patched guest transport, rather than
# merely verifying that its source patch applies.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-graphics.yml"
grep -Fq 'repo init -u https://android.googlesource.com/platform/manifest' "$workflow"
grep -Fq -- '--partial-clone --clone-filter=blob:limit=10M' "$workflow"
grep -Fq -- '--optimized-fetch' "$workflow"
grep -Fq 'cancel-in-progress: true' "$workflow"
grep -Fq 'dsa/graphics/fetch.sh .scratch/aosp/hardware/google/gfxstream' "$workflow"
grep -Fq 'm libOpenglSystemCommon' "$workflow"
grep -Fq 'libOpenglSystemCommon.so' "$workflow"
