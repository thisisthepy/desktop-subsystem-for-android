#!/usr/bin/env bash
# Asserts that no Microsoft WSA binaries or references exist in installer manifests
# or image build inputs, and that rejection logic functions correctly.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

if command -v uv >/dev/null 2>&1; then
    run_py() { uv run "$@"; }
elif command -v python3 >/dev/null 2>&1; then
    run_py() { python3 "$@"; }
else
    run_py() { python "$@"; }
fi

# Run self-test of the detector
run_py dsa/host/platform/windows/installer/check-no-wsa.py --self-test

# Check repository files
run_py dsa/host/platform/windows/installer/check-no-wsa.py \
    --manifests dsa/host/platform/windows/installer/dsa.wxs dsa/host/platform/windows/installer/bundle.wxs \
    --image-inputs dsa/guest/image/inputs.json

echo "ok    no Microsoft WSA binaries in payload or image inputs"
