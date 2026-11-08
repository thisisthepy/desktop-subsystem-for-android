#!/usr/bin/env bash
# The image CI boots is moved out of Cargo's target folder before the Rust build is restored.
# On a cache that was not an exact match (any changed Cargo.toml), rust-cache cleans the
# target folder and deletes every file in it that Cargo did not make: the image folder was
# left empty, and every boot check failed with "has neither system.vhdx nor disk.vhdx".
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-image.yml"
grep -Fq 'mv dsa/target/image/*/ .scratch/boot-image/' "$workflow"
grep -Fq 'image="$(ls -d "$PWD"/.scratch/boot-image/*/ | head -1)"' "$workflow"
if grep -q 'DSA_WIDGET_TEST_IMAGE=.*dsa/target' "$workflow" \
  || grep -Fq 'ls -d "$PWD"/dsa/target/image' "$workflow"; then
  echo "the boot checks read the image from Cargo's target folder, which rust-cache cleans"
  exit 1
fi
# The move comes before the Rust build is restored.
move=$(grep -n 'mv dsa/target/image/\*/ .scratch/boot-image/' "$workflow" | head -1 | cut -d: -f1)
cache=$(grep -n 'uses: Swatinem/rust-cache' "$workflow" | tail -1 | cut -d: -f1)
test "$move" -lt "$cache"
echo "image boot check image folder: ok"
