#!/usr/bin/env bash
# The PC measurement script: its arithmetic, checked without a PC running Android.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

if ! command -v pwsh >/dev/null; then
    echo "pwsh is not installed, so the measurement script cannot be checked" >&2
    exit 1
fi

pwsh -NoProfile -NonInteractive -File scripts/tests/measure_test.ps1
