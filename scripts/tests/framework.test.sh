#!/usr/bin/env bash
# The patched framework's tools and policy: the lock pins the published build each image is
# made from, the patches cover what it fetches and carry the shared recovery policy, the patch
# tool applies them exactly or stops, the collector describes the files for their image, and
# the recovery policy itself (a C++ test, built with the host's compiler). Building the
# framework is the "Framework build" workflow's job and is not run here; the image's half is in
# dsa/guest/image/test_make_image.py.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run dsa/guest/framework/test_framework_lock.py

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
c++ -std=c++17 -Wall -Wextra -Werror -I dsa/guest/framework/recovery \
  dsa/guest/framework/recovery/test_device_loss_recovery.cpp -o "$out/test_device_loss_recovery"
"$out/test_device_loss_recovery"

# The image release takes the artifacts the build keeps, by the same names.
grep -Fq 'name: framework-${{ matrix.android }}-${{ matrix.arch }}-${{ steps.pins.outputs.version }}' \
  .github/workflows/framework.yml
grep -Fq 'wanted+=("framework-$android-$arch-$version")' .github/workflows/release-image.yml
grep -Fq 'echo "args=--framework .scratch/framework" >> "$GITHUB_OUTPUT"' .github/workflows/release-image.yml
grep -Fq '${{ steps.framework.outputs.args }}' .github/workflows/release-image.yml
echo "framework build: ok"
