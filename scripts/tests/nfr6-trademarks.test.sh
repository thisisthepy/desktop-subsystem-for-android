#!/usr/bin/env bash
# Every user-facing name and notice follows the trademark rules: no Microsoft mark in a
# name, "Android" never first in a name, and the trademark and independence notices on the
# README files, the setup program, the MSI, the settings app and the APK install window.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run scripts/tests/nfr6_trademarks_test.py
uv run scripts/tests/nfr6_trademarks.py
