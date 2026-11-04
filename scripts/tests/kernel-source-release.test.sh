#!/usr/bin/env bash
# The release workflow publishes kernel-source.tar.gz in every release that ships the
# GPL-2.0 guest kernel, so the kernel's corresponding source always goes with it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run scripts/tests/kernel_source_release_test.py
uv run scripts/tests/kernel_source_release.py
